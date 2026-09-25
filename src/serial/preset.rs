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

/// 解析 HEX 字符串,允许空格/逗号/0x 前缀分隔,如 "AA 55,0x0F"
pub fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',')
        .collect();
    let cleaned = cleaned.replace("0x", "").replace("0X", "");
    if cleaned.is_empty() {
        return Ok(Vec::new());
    }
    if cleaned.len() % 2 != 0 {
        return Err(format!("HEX 字符数为奇数({} 个字符)", cleaned.len()));
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&cleaned[i..i + 2], 16)
                .map_err(|_| format!("非法 HEX 片段: {}", &cleaned[i..i + 2]))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_variants() {
        assert_eq!(decode_hex("AA 55").unwrap(), vec![0xAA, 0x55]);
        assert_eq!(decode_hex("aa,0x0f").unwrap(), vec![0xAA, 0x0F]);
        assert!(decode_hex("AAA").is_err());
        assert!(decode_hex("ZZ").is_err());
    }
}
