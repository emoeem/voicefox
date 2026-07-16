//! mg 签名工具

use std::time::{UNIX_EPOCH, SystemTime};

use super::super::crypto;

/// 生成咪咕搜索签名
pub fn mg_sign(keyword: &str) -> (String, String) {
    let device_id = "963B7AA0D21511ED807EE5846EC87D20";
    let signature_md5 = "6cdc72a439cef99a3418d2a78aa28c73";

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string();

    // 第一次 MD5
    let sign = format!("{}{}{}", keyword, signature_md5, "yyapp2d16148780a1dcc7408e06336b98cfd50");
    let sign = crypto::md5(&sign);

    // 第二次 MD5
    let sign = format!("{}{}{}", sign, device_id, timestamp);
    let sign = crypto::md5(&sign);

    (sign, timestamp)
}
