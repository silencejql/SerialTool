//! 发送预设:多条数据预设,支持 HEX/ASCII、附加 CRLF、定时重发
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataFormat {
    Ascii,
    Hex,
}

impl Default for DataFormat {
    fn default() -> Self {
        DataFormat::Ascii
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SendPreset {
    #[serde(default)]
    pub id: u64,
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub format: DataFormat,
    #[serde(default = "default_true")]
    pub append_crlf: bool,
    /// None = 不重发;Some(毫秒) = 定时重发周期
    #[serde(default)]
    pub repeat_interval_ms: Option<u32>,
    #[serde(default)]
    pub enabled: bool,
    /// 勾选=在预设列表中内联展开显示并可直接编辑内容;取消=只显示预设名称
    #[serde(default)]
    pub expanded: bool,
}

fn default_true() -> bool {
    true
}

impl SendPreset {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            name: "新预设".into(),
            content: String::new(),
            format: DataFormat::Ascii,
            append_crlf: true,
            repeat_interval_ms: None,
            enabled: false,
            expanded: false,
        }
    }

    /// 按格式解码内容,可选追加 CRLF
    pub fn decode(&self) -> Result<Vec<u8>, String> {
        let mut data = match self.format {
            DataFormat::Ascii => self.content.as_bytes().to_vec(),
            DataFormat::Hex => decode_hex(&self.content)?,
        };
        if self.append_crlf {
            data.extend_from_slice(b"\r\n");
        }
        Ok(data)
    }
}

/// 解析 HEX 字符串,允许空格/逗号分隔,每个片段可带一次 0x 前缀,如 "AA 55,0x0F";
/// 也允许不加分隔符连写,如 "AA550F"。
pub fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for token in s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
    {
        // 只去掉片段开头的一次 0x/0X;旧实现全局 replace 会误删字节中间的 0x
        let hex = token
            .strip_prefix("0x")
            .or_else(|| token.strip_prefix("0X"))
            .unwrap_or(token);
        if hex.is_empty() {
            return Err(format!("存在空的 HEX 片段: \"{token}\""));
        }
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("非法 HEX 片段: \"{token}\""));
        }
        if hex.len() % 2 != 0 {
            return Err(format!("HEX 片段字符数为奇数: \"{token}\""));
        }
        for i in (0..hex.len()).step_by(2) {
            // 上面已逐字符校验为十六进制,此处不会失败
            out.push(u8::from_str_radix(&hex[i..i + 2], 16).unwrap());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_variants() {
        assert_eq!(decode_hex("AA 55").unwrap(), vec![0xAA, 0x55]);
        assert_eq!(decode_hex("aa,0x0f").unwrap(), vec![0xAA, 0x0F]);
        // 无分隔符连写
        assert_eq!(decode_hex("AA550F").unwrap(), vec![0xAA, 0x55, 0x0F]);
        // 大小写 0X 前缀混用、连续逗号
        assert_eq!(
            decode_hex("0xAA, 0X55,,0x0f").unwrap(),
            vec![0xAA, 0x55, 0x0F]
        );
        // 空白/空字符串解析为零字节
        assert!(decode_hex("").unwrap().is_empty());
        assert!(decode_hex("  ,  ").unwrap().is_empty());
    }

    #[test]
    fn hex_rejects_bad_input() {
        assert!(decode_hex("AAA").is_err());
        assert!(decode_hex("ZZ").is_err());
        assert!(decode_hex("AA 5").is_err()); // 片段奇数长度
        assert!(decode_hex("0x").is_err()); // 空前缀
        // 字节中间出现 0x 不得被静默删除(旧实现会错误解析成功)
        assert!(decode_hex("0A0x55").is_err());
    }
}
