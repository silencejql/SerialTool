//! Agent → 监控端二进制帧协议解析
//!
//! 帧格式(小端):`A5 5A | type:u8 | pid:u32 | port_len:u16 | port utf8 | data_len:u32 | data`

pub const FT_ATTACH: u8 = 1;
pub const FT_RX: u8 = 2;
pub const FT_TX: u8 = 3;
pub const FT_INFO: u8 = 4;

pub const CMD_DETACH: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub ftype: u8,
    pub pid: u32,
    pub port: String,
    pub data: Vec<u8>,
}

pub struct FrameParser {
    buf: Vec<u8>,
}

impl FrameParser {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(4096) }
    }

    /// 追加字节流,返回已解析出的完整帧
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 2 {
                break;
            }
            // 找 magic
            let start = match self.buf.windows(2).position(|w| w == [0xA5, 0x5A]) {
                Some(i) => i,
                None => {
                    self.buf.clear();
                    break;
                }
            };
            if start > 0 {
                self.buf.drain(..start);
            }
            // 固定头到 port_len 为止需 9 字节:magic(2)+type(1)+pid(4)+plen(2)
            if self.buf.len() < 9 {
                break;
            }
            let plen = u16::from_le_bytes([self.buf[7], self.buf[8]]) as usize;
            // dlen 紧跟 port 之后:9 + plen
            if self.buf.len() < 9 + plen + 4 {
                break;
            }
            let dlen = u32::from_le_bytes([
                self.buf[9 + plen],
                self.buf[10 + plen],
                self.buf[11 + plen],
                self.buf[12 + plen],
            ]) as usize;
            // 9 固定头 + plen port + 4 dlen + data(13 + plen + dlen)
            let total = match 13usize.checked_add(plen).and_then(|v| v.checked_add(dlen)) {
                Some(t) if t <= 16 * 1024 * 1024 => t,
                Some(_) => {
                    // 异常大帧:丢弃 magic 重新同步
                    self.buf.drain(..2);
                    continue;
                }
                None => {
                    self.buf.drain(..2);
                    continue;
                }
            };
            if self.buf.len() < total {
                break;
            }
            let ftype = self.buf[2];
            let pid = u32::from_le_bytes([
                self.buf[3],
                self.buf[4],
                self.buf[5],
                self.buf[6],
            ]);
            let port = String::from_utf8_lossy(&self.buf[9..9 + plen]).into_owned();
            let data = self.buf[13 + plen..total].to_vec();
            out.push(Frame {
                ftype,
                pid,
                port,
                data,
            });
            self.buf.drain(..total);
        }
        out
    }
}

/// 构造下行命令帧
pub fn command(cmd: u8) -> [u8; 3] {
    [0xA5, 0x5A, cmd]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(ftype: u8, pid: u32, port: &str, data: &[u8]) -> Vec<u8> {
        let mut v = vec![0xA5, 0x5A, ftype];
        v.extend_from_slice(&pid.to_le_bytes());
        v.extend_from_slice(&(port.len() as u16).to_le_bytes());
        v.extend_from_slice(port.as_bytes());
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn parse_full_frame() {
        let raw = encode(FT_TX, 1234, "COM10", &[0xAA, 0x55, 0x01]);
        let mut p = FrameParser::new();
        // 分两次喂入
        let (a, b) = raw.split_at(6);
        assert!(p.push(a).is_empty());
        let frames = p.push(b);
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0],
            Frame {
                ftype: FT_TX,
                pid: 1234,
                port: "COM10".into(),
                data: vec![0xAA, 0x55, 0x01],
            }
        );
    }

    #[test]
    fn parse_coalesced_frames_with_ports() {
        // 非空 port 的多帧粘连(agent 批量 flush 的真实形态)
        let mut raw = encode(FT_TX, 7, "COM10", b"PING 1\r\n");
        raw.extend(encode(FT_INFO, 7, "", b"hooks all enabled"));
        raw.extend(encode(FT_RX, 7, "COM10", &[0x68, 0x69]));
        let mut p = FrameParser::new();
        // 每次只喂 3 字节,强制跨多次读分片
        let mut frames = Vec::new();
        for chunk in raw.chunks(3) {
            frames.extend(p.push(chunk));
        }
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].port, "COM10");
        assert_eq!(frames[0].data, b"PING 1\r\n");
        assert_eq!(frames[1].port, "");
        assert_eq!(frames[1].data, b"hooks all enabled");
        assert_eq!(frames[2].ftype, FT_RX);
        assert_eq!(frames[2].data, &[0x68, 0x69]);
    }

    #[test]
    fn parse_multiple_with_garbage() {
        let mut raw = vec![0x00, 0x11]; // 前导垃圾
        raw.extend(encode(FT_RX, 1, "COM3", b"hi"));
        raw.extend(encode(FT_ATTACH, 2, "", b""));
        let mut p = FrameParser::new();
        let frames = p.push(&raw);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, b"hi");
        assert_eq!(frames[1].ftype, FT_ATTACH);
        assert!(frames[1].port.is_empty());
    }
}
