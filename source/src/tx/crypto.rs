//! QQ音乐 zzcSign 签名算法
//!
//! 参考: lx-music src/renderer/utils/musicSdk/tx/

/// zzcSign 签名算法
/// 输入: data (JSON 字符串)
/// 输出: 签名字符串
pub fn zzc_sign(data: &str) -> String {
    use sha1::Digest;

    // 1. SHA1(data)
    let hash = format!("{:x}", sha1::Sha1::digest(data.as_bytes()));

    // 2. 从 hash 特定位置取字符组成 part1
    let indexes1 = [23, 14, 6, 36, 16, 40, 7, 19];
    let part1: String = indexes1
        .iter()
        .filter_map(|&i| hash.chars().nth(i))
        .collect();

    // 3. 从 hash 特定位置取字符组成 part2
    let indexes2 = [16, 1, 32, 12, 19, 27, 8, 5];
    let part2: String = indexes2
        .iter()
        .filter_map(|&i| hash.chars().nth(i))
        .collect();

    // 4. XOR scramble
    let scramble_values: [u32; 20] = [
        89, 39, 179, 150, 218, 82, 58, 252, 177, 52, 186, 123, 120, 64, 242, 133, 143, 161, 121,
        179,
    ];

    let mut part3_bytes = Vec::with_capacity(20);
    for i in 0..20 {
        let byte_val = if i * 2 + 1 < hash.len() {
            u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16).unwrap_or(0)
        } else {
            0
        };
        let xor_val = (scramble_values[i] as u8) ^ byte_val;
        part3_bytes.push(xor_val);
    }

    // 5. base64 encode part3
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&part3_bytes);
    let b64_clean: String = b64
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | '+' | '='))
        .collect();

    // 6. 组合
    format!("zzc{}{}{}", part1, b64_clean, part2).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zzc_sign_non_empty() {
        let data = r#"{"test":"hello"}"#;
        let sign = zzc_sign(data);
        assert_eq!(sign, "zzc8cbd808brhsjkhrcde4fogzsjczmgmobi8c73809f");
    }
}
