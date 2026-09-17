//! 汽水音乐加密音频解密（MP4 CENC）。
//!
//! 与 music-lib 的 `soda/crypto.go` 对齐：
//! 1. `playAuth` 是 base64，先去掉尾部填充，再逐字节还原「spade」内层，
//!    最后按 base36 跳过前缀得到十六进制密钥；
//! 2. 音频是 CENC 加密的 MP4：`stsz` 给出每个样本的长度，`senc` 给出每个样本的
//!    IV（以及可选的子样本划分），按 AES-CTR 逐样本解密 `mdat`；
//! 3. 解密后把 `stsd` 里的 `enca` 改回原始编码（`frma` 里记录的原格式）。

use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use aes::{Aes128, Aes192, Aes256};
use base64::Engine;

/// 逐样本解密所需的 AES 实例（密钥长度不定）。
enum Cipher {
    Aes128(Box<Aes128>),
    Aes192(Box<Aes192>),
    Aes256(Box<Aes256>),
}

impl Cipher {
    fn new(key: &[u8]) -> Result<Self, String> {
        match key.len() {
            16 => Ok(Cipher::Aes128(Box::new(Aes128::new(
                GenericArray::from_slice(key),
            )))),
            24 => Ok(Cipher::Aes192(Box::new(Aes192::new(
                GenericArray::from_slice(key),
            )))),
            32 => Ok(Cipher::Aes256(Box::new(Aes256::new(
                GenericArray::from_slice(key),
            )))),
            other => Err(format!("不支持的 AES 密钥长度: {other}")),
        }
    }

    fn encrypt_block(&self, block: &mut [u8; 16]) {
        let mut array = GenericArray::clone_from_slice(block);
        match self {
            Cipher::Aes128(cipher) => cipher.encrypt_block(&mut array),
            Cipher::Aes192(cipher) => cipher.encrypt_block(&mut array),
            Cipher::Aes256(cipher) => cipher.encrypt_block(&mut array),
        }
        block.copy_from_slice(&array);
    }
}

/// Go 的 `cipher.NewCTR` 把整个 16 字节计数器当大端整数递增，这里保持一致。
struct CtrStream<'a> {
    cipher: &'a Cipher,
    counter: [u8; 16],
}

impl<'a> CtrStream<'a> {
    fn new(cipher: &'a Cipher, iv: &[u8]) -> Self {
        let mut counter = [0u8; 16];
        let len = iv.len().min(16);
        counter[..len].copy_from_slice(&iv[..len]);
        Self { cipher, counter }
    }

    fn apply(&mut self, data: &mut [u8]) {
        for chunk in data.chunks_mut(16) {
            let mut keystream = self.counter;
            self.cipher.encrypt_block(&mut keystream);
            for (byte, key) in chunk.iter_mut().zip(keystream.iter()) {
                *byte ^= key;
            }
            for index in (0..16).rev() {
                let (value, overflow) = self.counter[index].overflowing_add(1);
                self.counter[index] = value;
                if !overflow {
                    break;
                }
            }
        }
    }
}

/// MP4 盒子（起始偏移、总长度、内容起点）。
#[derive(Debug, Clone, Copy)]
struct Mp4Box {
    offset: usize,
    size: usize,
    data_start: usize,
}

impl Mp4Box {
    fn data<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[self.data_start..self.offset + self.size]
    }
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_be_bytes(bytes.try_into().ok()?))
}

/// 在 `[start, end)` 的同级盒子里查找指定类型。
fn find_box(data: &[u8], box_type: &[u8; 4], start: usize, end: usize) -> Option<Mp4Box> {
    let end = end.min(data.len());
    let mut pos = start;
    while pos + 8 <= end {
        let size = read_u32(data, pos)? as usize;
        if size < 8 || pos + size > end {
            break;
        }
        if data.get(pos + 4..pos + 8) == Some(box_type) {
            return Some(Mp4Box {
                offset: pos,
                size,
                data_start: pos + 8,
            });
        }
        pos += size;
    }
    None
}

/// 递归查找：只对已知的容器盒子下钻。
fn find_box_deep(data: &[u8], box_type: &[u8; 4], start: usize, end: usize) -> Option<Mp4Box> {
    let end = end.min(data.len());
    let mut pos = start;
    while pos + 8 <= end {
        let mut size = read_u32(data, pos)? as usize;
        let mut header = 8usize;
        if size == 1 {
            let bytes = data.get(pos + 8..pos + 16)?;
            let size64 = u64::from_be_bytes(bytes.try_into().ok()?);
            if size64 > (end - pos) as u64 {
                break;
            }
            size = size64 as usize;
            header = 16;
        }
        if size < header || pos + size > end {
            break;
        }
        if data.get(pos + 4..pos + 8) == Some(box_type) {
            return Some(Mp4Box {
                offset: pos,
                size,
                data_start: pos + header,
            });
        }
        if let Some(child_start) = box_child_start(data, pos, header)
            && child_start < pos + size
            && let Some(found) = find_box_deep(data, box_type, child_start, pos + size)
        {
            return Some(found);
        }
        pos += size;
    }
    None
}

/// 已知容器盒子的子盒子起点。
fn box_child_start(data: &[u8], offset: usize, header: usize) -> Option<usize> {
    let box_type = data.get(offset + 4..offset + 8)?;
    match box_type {
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"sinf" | b"schi" => {
            Some(offset + header)
        }
        b"stsd" => Some(offset + header + 8),
        b"enca" | b"mp4a" | b"alac" | b"fLaC" => Some(offset + header + 28),
        _ => None,
    }
}

/// `stsz`：固定样本长度或逐样本长度表。
fn parse_stsz(data: &[u8]) -> Vec<u32> {
    if data.len() < 12 {
        return Vec::new();
    }
    let fixed = u32::from_be_bytes(data[4..8].try_into().unwrap_or_default());
    let count = u32::from_be_bytes(data[8..12].try_into().unwrap_or_default()) as usize;
    if fixed != 0 {
        return vec![fixed; count];
    }
    (0..count)
        .map(|index| read_u32(data, 12 + index * 4).unwrap_or_default())
        .collect()
}

#[derive(Debug, Clone, Copy, Default)]
struct SencSubsample {
    clear: u16,
    encrypted: u32,
}

#[derive(Debug, Clone)]
struct SencSample {
    iv: Vec<u8>,
    subsamples: Vec<SencSubsample>,
}

fn parse_senc(data: &[u8], iv_size: usize) -> Vec<SencSample> {
    if data.len() < 8 {
        return Vec::new();
    }
    let iv_size = if iv_size == 8 || iv_size == 16 {
        iv_size
    } else {
        8
    };
    let flags = read_u32(data, 0).unwrap_or_default() & 0x00FF_FFFF;
    let count = read_u32(data, 4).unwrap_or_default() as usize;
    let has_subsamples = flags & 0x02 != 0;
    let mut samples = Vec::with_capacity(count);
    let mut ptr = 8usize;
    for _ in 0..count {
        if ptr + iv_size > data.len() {
            break;
        }
        let mut sample = SencSample {
            iv: data[ptr..ptr + iv_size].to_vec(),
            subsamples: Vec::new(),
        };
        ptr += iv_size;
        if has_subsamples {
            let Some(sub_count) = read_u16(data, ptr).map(usize::from) else {
                break;
            };
            ptr += 2;
            if ptr + sub_count * 6 > data.len() {
                break;
            }
            for _ in 0..sub_count {
                sample.subsamples.push(SencSubsample {
                    clear: read_u16(data, ptr).unwrap_or_default(),
                    encrypted: read_u32(data, ptr + 2).unwrap_or_default(),
                });
                ptr += 6;
            }
        }
        samples.push(sample);
    }
    samples
}

/// `tenc` 里的 IV 长度（第 7 字节），缺省 8。
fn default_iv_size(data: &[u8], start: usize, end: usize) -> usize {
    match find_box_deep(data, b"tenc", start, end) {
        Some(tenc) => match tenc.data(data).get(7).copied() {
            Some(8) | Some(16) => tenc.data(data)[7] as usize,
            _ => 8,
        },
        None => 8,
    }
}

/// 解密单个样本：先跳过明文子样本，再对加密段做 CTR。
fn decrypt_sample(cipher: &Cipher, chunk: &[u8], sample: &SencSample) -> Vec<u8> {
    let mut stream = CtrStream::new(cipher, &sample.iv);
    if sample.subsamples.is_empty() {
        let mut output = chunk.to_vec();
        stream.apply(&mut output);
        return output;
    }
    let mut output = chunk.to_vec();
    let mut pos = 0usize;
    for sub in &sample.subsamples {
        let clear = (sub.clear as usize).min(output.len().saturating_sub(pos));
        pos += clear;
        if pos >= output.len() {
            break;
        }
        let encrypted = (sub.encrypted as usize).min(output.len() - pos);
        stream.apply(&mut output[pos..pos + encrypted]);
        pos += encrypted;
        if pos >= output.len() {
            break;
        }
    }
    output
}

/// `enca` 样本条目里记录的原始编码（`frma` 的载荷）。
fn original_sample_format(stsd_data: &[u8]) -> [u8; 4] {
    let Some(index) = stsd_data.windows(4).position(|window| window == b"frma") else {
        return *b"mp4a";
    };
    if index < 4 || index + 8 > stsd_data.len() {
        return *b"mp4a";
    }
    let size =
        u32::from_be_bytes(stsd_data[index - 4..index].try_into().unwrap_or_default()) as usize;
    if size < 12 || index - 4 + size > stsd_data.len() {
        return *b"mp4a";
    }
    stsd_data[index + 4..index + 8]
        .try_into()
        .unwrap_or(*b"mp4a")
}

/// 取出 `playAuth` 里的十六进制密钥。
pub(super) fn extract_key(play_auth: &str) -> Result<String, String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(play_auth.trim())
        .map_err(|error| format!("playAuth 不是合法 base64: {error}"))?;
    if decoded.len() < 3 {
        return Err("playAuth 数据过短".to_string());
    }
    let padding = decoded[0] ^ decoded[1] ^ decoded[2];
    let padding = padding.checked_sub(48).ok_or("playAuth 填充长度非法")? as usize;
    if decoded.len() < padding + 2 {
        return Err("playAuth 填充长度越界".to_string());
    }
    let inner = &decoded[1..decoded.len() - padding];
    let restored = spade_inner(inner);
    let Some(&first) = restored.first() else {
        return Err("playAuth 还原失败".to_string());
    };
    let skip = decode_base36(first);
    let end = 1 + (decoded.len() - padding - 2);
    let end = end.saturating_sub(skip);
    if end > restored.len() || end < 1 {
        return Err("playAuth 索引越界".to_string());
    }
    String::from_utf8(restored[1..end].to_vec()).map_err(|error| error.to_string())
}

/// spade 内层还原：`v = (byte ^ key) - bitcount(i) - 21 (mod 255)`，
/// 其中异或用的 `key` 由「固定两字节 + 自身」构成。
fn spade_inner(key_bytes: &[u8]) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(key_bytes.len() + 2);
    buffer.push(0xFA);
    buffer.push(0x55);
    buffer.extend_from_slice(key_bytes);
    key_bytes
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            let value = i32::from(*byte ^ buffer[index]) - bitcount(index) - 21;
            value.rem_euclid(255) as u8
        })
        .collect()
}

fn bitcount(value: usize) -> i32 {
    (value as u32).count_ones() as i32
}

fn decode_base36(byte: u8) -> usize {
    match byte {
        b'0'..=b'9' => usize::from(byte - b'0'),
        b'a'..=b'z' => usize::from(byte - b'a' + 10),
        _ => 0xFF,
    }
}

/// 解密整段 CENC 加密的 MP4。
pub(super) fn decrypt_audio(file_data: &[u8], play_auth: &str) -> Result<Vec<u8>, String> {
    let key_hex = extract_key(play_auth)?;
    let key = hex::decode(key_hex.trim()).map_err(|error| format!("密钥不是十六进制: {error}"))?;
    let cipher = Cipher::new(&key)?;

    let moov = find_box(file_data, b"moov", 0, file_data.len())
        .ok_or_else(|| "未找到 moov 盒子".to_string())?;
    let stbl = find_box(file_data, b"stbl", moov.data_start, moov.offset + moov.size)
        .or_else(|| find_box_deep(file_data, b"stbl", moov.data_start, moov.offset + moov.size))
        .ok_or_else(|| "未找到 stbl 盒子".to_string())?;

    let stsz = find_box(file_data, b"stsz", stbl.data_start, stbl.offset + stbl.size)
        .ok_or_else(|| "未找到 stsz 盒子".to_string())?;
    let sample_sizes = parse_stsz(stsz.data(file_data));

    let senc = find_box(file_data, b"senc", moov.data_start, moov.offset + moov.size)
        .or_else(|| find_box(file_data, b"senc", stbl.data_start, stbl.offset + stbl.size))
        .ok_or_else(|| "未找到 senc 盒子".to_string())?;
    let iv_size = default_iv_size(file_data, stbl.offset, stbl.offset + stbl.size);
    let senc_samples = parse_senc(senc.data(file_data), iv_size);

    let mdat = find_box(file_data, b"mdat", 0, file_data.len())
        .ok_or_else(|| "未找到 mdat 盒子".to_string())?;

    let mut output = file_data.to_vec();
    let mut read_ptr = mdat.data_start;
    let mut decrypted: Vec<u8> = Vec::with_capacity(mdat.size.saturating_sub(8));
    for (index, size) in sample_sizes.iter().enumerate() {
        let size = *size as usize;
        if read_ptr + size > output.len() {
            break;
        }
        let chunk = output[read_ptr..read_ptr + size].to_vec();
        match senc_samples.get(index) {
            Some(sample) => decrypted.extend_from_slice(&decrypt_sample(&cipher, &chunk, sample)),
            None => decrypted.extend_from_slice(&chunk),
        }
        read_ptr += size;
    }
    if decrypted.len() != mdat.size.saturating_sub(8) {
        return Err("解密后长度与 mdat 不一致".to_string());
    }
    output[mdat.data_start..mdat.offset + mdat.size].copy_from_slice(&decrypted);

    // 把 `enca` 改回原始编码，播放器才会按普通音频处理。
    if let Some(stsd) = find_box(file_data, b"stsd", stbl.data_start, stbl.offset + stbl.size)
        && let Some(index) = stsd
            .data(file_data)
            .windows(4)
            .position(|window| window == b"enca")
    {
        let replacement = original_sample_format(stsd.data(file_data));
        let start = stsd.data_start + index;
        output[start..start + 4].copy_from_slice(&replacement);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按 spade 的逆运算构造一个合法的 `playAuth`，用于回归解密链路。
    ///
    /// 目标还原结果是 `['0'] + 密钥字节`：`'0'` 在 base36 里表示跳过 0 字节，
    /// 这样 `extract_key` 正好从第 1 位截到末尾，拿到的就是密钥。
    fn build_play_auth(key_hex: &str, padding: u8) -> String {
        let mut restored = vec![b'0'];
        restored.extend_from_slice(key_hex.as_bytes());
        let mut inner = Vec::with_capacity(restored.len());
        for (index, value) in restored.iter().enumerate() {
            let buff = if index == 0 {
                0xFA
            } else if index == 1 {
                0x55
            } else {
                inner[index - 2]
            };
            let encoded = (i32::from(*value) + bitcount(index) + 21).rem_euclid(255) as u8;
            inner.push(encoded ^ buff);
        }
        // 前三位异或结果决定填充长度。
        let first = (padding + 48) ^ inner[0] ^ inner[1];
        let mut decoded = vec![first];
        decoded.extend_from_slice(&inner);
        decoded.extend(std::iter::repeat_n(0u8, padding as usize));
        base64::engine::general_purpose::STANDARD.encode(decoded)
    }

    #[test]
    fn ctr_matches_the_nist_vector() {
        // NIST SP 800-38A AES-128-CTR 第一块。
        let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let iv = hex::decode("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff").unwrap();
        let cipher = Cipher::new(&key).unwrap();
        let mut stream = CtrStream::new(&cipher, &iv);
        let mut data = hex::decode("6bc1bee22e409f96e93d7e117393172a").unwrap();
        stream.apply(&mut data);
        assert_eq!(hex::encode(data), "874d6191b620e3261bef6864990db6ce");
    }

    #[test]
    fn play_auth_round_trips_to_the_key() {
        let key = "00112233445566778899aabbccddeeff";
        let play_auth = build_play_auth(key, 3);
        assert_eq!(extract_key(&play_auth).unwrap(), key);
    }

    #[test]
    fn rejects_invalid_play_auth() {
        assert!(extract_key("not-base64!!").is_err());
        assert!(extract_key("QUJD").is_err(), "过短的载荷应当被拒绝");
    }

    #[test]
    fn base36_and_bitcount_match_the_reference() {
        assert_eq!(decode_base36(b'0'), 0);
        assert_eq!(decode_base36(b'a'), 10);
        assert_eq!(decode_base36(b'z'), 35);
        assert_eq!(bitcount(0), 0);
        assert_eq!(bitcount(7), 3);
        assert_eq!(bitcount(255), 8);
    }

    #[test]
    fn sample_decryption_skips_clear_subsamples() {
        let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let cipher = Cipher::new(&key).unwrap();
        let sample = SencSample {
            iv: hex::decode("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff").unwrap(),
            subsamples: vec![SencSubsample {
                clear: 2,
                encrypted: 6,
            }],
        };
        let chunk = hex::decode("0000c1bee22e409f960000").unwrap();
        let output = decrypt_sample(&cipher, &chunk, &sample);
        // 前 2 字节是明文子样本，保持不动。
        assert_eq!(&output[..2], &[0, 0]);
        let expected = {
            let mut stream = CtrStream::new(&cipher, &sample.iv);
            let mut data = hex::decode("c1bee22e409f").unwrap();
            stream.apply(&mut data);
            data
        };
        assert_eq!(&output[2..8], expected.as_slice());
    }

    #[test]
    fn sample_format_reads_frma() {
        let stsd = hex::decode("0000000c66726d616d703461").unwrap();
        assert_eq!(original_sample_format(&stsd), *b"mp4a");
        assert_eq!(original_sample_format(&[0u8; 4]), *b"mp4a");
    }

    #[test]
    fn stsz_supports_fixed_and_per_sample_sizes() {
        // 固定长度：size=0, sample_size=100, count=3
        let fixed = hex::decode("000000000000006400000003").unwrap();
        assert_eq!(parse_stsz(&fixed), vec![100, 100, 100]);
        // 逐样本：size=0, sample_size=0, count=2, [11, 22]
        let per_sample = hex::decode("0000000000000000000000020000000b00000016").unwrap();
        assert_eq!(parse_stsz(&per_sample), vec![11, 22]);
    }
}
