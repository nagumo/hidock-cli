//! HiDock の USB プロトコル（Jensen）。読み取り系のコマンドだけを実装する。
//!
//! パケット: 0x12 0x34 | cmd u16 BE | seq u32 BE | len u32 BE | body
//! 受信の len は上位 8bit がチェックサム長、下位 24bit が本体長。
//! 参考実装: sgeraldes/hidock-light apps/desktop/src/hidock_device.py

use anyhow::{Context, Result, anyhow, bail};
use rusb::{Context as UsbContext, DeviceHandle, UsbContext as _};
use std::time::{Duration, Instant};

pub const VENDOR_IDS: [u16; 2] = [0x10D6, 0x3887];
pub const PRODUCT_IDS: [(u16, &str); 13] = [
    (0xAF0C, "H1"),
    (0xB00C, "H1"),
    (0xAF0D, "H1E"),
    (0xB00D, "H1E"),
    (0xAF0E, "P1"),
    (0xB00E, "P1"),
    (0xAF0F, "P1 mini"),
    (0x0100, "H1"),
    (0x0101, "H1E"),
    (0x0102, "H1"),
    (0x0103, "H1E"),
    (0x2040, "P1"),
    (0x2041, "P1 mini"),
];

const EP_OUT: u8 = 0x01;
const EP_IN: u8 = 0x82;
const INTERFACE: u8 = 0;

const CMD_GET_DEVICE_INFO: u16 = 1;
const CMD_GET_FILE_LIST: u16 = 4;
const CMD_TRANSFER_FILE: u16 = 5;
// 削除(7)・フォーマット(17)などの書き込み系コマンドは意図的に実装しない。

pub struct Packet {
    pub cmd: u16,

    pub body: Vec<u8>,
}

pub struct DeviceInfo {
    pub version: String,
    pub serial: String,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub length: u32,

}

pub struct Jensen {
    handle: DeviceHandle<UsbContext>,
    seq: u32,
    buf: Vec<u8>,
    read_size: usize,
    pub model: &'static str,
    pub pid: u16,
}

impl Jensen {
    pub fn open() -> Result<Self> {
        let ctx = UsbContext::new()?;
        for dev in ctx.devices()?.iter() {
            let d = dev.device_descriptor()?;
            if !VENDOR_IDS.contains(&d.vendor_id()) {
                continue;
            }
            let Some(&(pid, model)) = PRODUCT_IDS.iter().find(|(p, _)| *p == d.product_id()) else {
                continue;
            };
            let handle = dev.open().context("HiDock を開けない")?;
            // macOS では設定済みの機器に set_configuration すると失敗するので、未設定のときだけ行う。
            if handle.active_configuration().unwrap_or(0) == 0 {
                handle.set_active_configuration(1).context("set_configuration に失敗")?;
            }
            let _ = handle.set_auto_detach_kernel_driver(true);
            handle.claim_interface(INTERFACE).map_err(|e| {
                anyhow!(
                    "インターフェースを確保できない: {e}\n\
                     → Chrome（HiNotes）や GUI 版が HiDock を掴んでいないか確認（Chrome は ⌘Q で完全に終了）"
                )
            })?;
            let mps = dev
                .active_config_descriptor()
                .ok()
                .and_then(|c| {
                    c.interfaces()
                        .flat_map(|i| i.descriptors().collect::<Vec<_>>())
                        .flat_map(|i| i.endpoint_descriptors().collect::<Vec<_>>())
                        .find(|e| e.address() == EP_IN)
                        .map(|e| e.max_packet_size() as usize)
                })
                .unwrap_or(512);
            return Ok(Self { handle, seq: 0, buf: Vec::new(), read_size: mps * 64, model, pid });
        }
        bail!("HiDock が見つからない（USB でつながっているか確認）")
    }

    fn send(&mut self, cmd: u16, body: &[u8]) -> Result<u32> {
        self.seq = self.seq.wrapping_add(1);
        let mut pkt = Vec::with_capacity(12 + body.len());
        pkt.extend_from_slice(&[0x12, 0x34]);
        pkt.extend_from_slice(&cmd.to_be_bytes());
        pkt.extend_from_slice(&self.seq.to_be_bytes());
        pkt.extend_from_slice(&(body.len() as u32).to_be_bytes());
        pkt.extend_from_slice(body);
        let n = self.handle.write_bulk(EP_OUT, &pkt, Duration::from_secs(10))?;
        if n != pkt.len() {
            bail!("送信が途中で切れた（{n}/{} bytes）", pkt.len());
        }
        Ok(self.seq)
    }

    /// バッファから1パケット取り出す。同期バイトがずれていたら読み飛ばす。
    fn take_packet(&mut self) -> Option<Packet> {
        loop {
            if self.buf.len() < 2 {
                return None;
            }
            if self.buf[0] != 0x12 || self.buf[1] != 0x34 {
                match self.buf.windows(2).position(|w| w == [0x12, 0x34]) {
                    Some(off) => {
                        self.buf.drain(..off);
                    }
                    None => {
                        self.buf.clear();
                        return None;
                    }
                }
            }
            if self.buf.len() < 12 {
                return None;
            }
            let cmd = u16::from_be_bytes([self.buf[2], self.buf[3]]);

            let len_field = u32::from_be_bytes(self.buf[8..12].try_into().unwrap());
            let body_len = (len_field & 0x00FF_FFFF) as usize;
            let checksum_len = (len_field >> 24) as usize;
            let total = 12 + body_len + checksum_len;
            if self.buf.len() < total {
                return None;
            }
            let body = self.buf[12..12 + body_len].to_vec();
            self.buf.drain(..total);
            return Some(Packet { cmd, body });
        }
    }

    /// `want_cmd` のパケットを1つ待つ。時間切れなら None。
    fn recv(&mut self, want_cmd: u16, timeout: Duration) -> Result<Option<Packet>> {
        let deadline = Instant::now() + timeout;
        let mut chunk = vec![0u8; self.read_size];
        loop {
            while let Some(p) = self.take_packet() {
                if p.cmd == want_cmd {
                    return Ok(Some(p));
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            match self.handle.read_bulk(EP_IN, &mut chunk, Duration::from_millis(200)) {
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(rusb::Error::Timeout) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// 失敗後に残ったデータを読み捨てて、次のコマンドに備える。
    fn flush(&mut self) {
        self.buf.clear();
        let mut chunk = vec![0u8; self.read_size];
        for _ in 0..20 {
            match self.handle.read_bulk(EP_IN, &mut chunk, Duration::from_millis(50)) {
                Ok(n) if n > 0 => continue,
                _ => break,
            }
        }
    }

    pub fn device_info(&mut self) -> Result<DeviceInfo> {
        self.buf.clear();
        self.send(CMD_GET_DEVICE_INFO, &[])?;
        let p = self
            .recv(CMD_GET_DEVICE_INFO, Duration::from_secs(5))?
            .ok_or_else(|| anyhow!("機器情報の応答がない"))?;
        if p.body.len() < 4 {
            bail!("機器情報が短すぎる（{} bytes）", p.body.len());
        }
        if std::env::var_os("HIDOCK_DEBUG").is_some() {
            eprintln!("device_info body ({} bytes): {:02x?}", p.body.len(), p.body);
        }
        let version = p.body[1..4].iter().map(|b| b.to_string()).collect::<Vec<_>>().join(".");
        let serial: String = p.body[4..p.body.len().min(20)].iter()
            .take_while(|&&b| b != 0)
            .filter(|b| (32..=126).contains(*b))
            .map(|&b| b as char)
            .collect();
        Ok(DeviceInfo { version, serial })
    }

    pub fn list_files(&mut self) -> Result<Vec<FileEntry>> {
        self.buf.clear();
        self.send(CMD_GET_FILE_LIST, &[])?;
        let mut data = Vec::new();
        let mut timeouts = 0;
        loop {
            match self.recv(CMD_GET_FILE_LIST, Duration::from_millis(1000))? {
                Some(p) if p.body.is_empty() => break,
                Some(p) => {
                    timeouts = 0;
                    data.extend_from_slice(&p.body);
                    // 先頭の 0xFFFF + 件数が分かっていれば、件数がそろった時点で終わる。
                    if let Some(expected) = header_count(&data) {
                        if parse_file_list(&data).len() >= expected {
                            break;
                        }
                    }
                }
                None => {
                    timeouts += 1;
                    if timeouts >= 3 {
                        break;
                    }
                }
            }
        }
        let files = parse_file_list(&data);
        if let Some(expected) = header_count(&data) {
            if files.len() < expected {
                eprintln!("⚠️ 一覧が途中まで: {}/{expected} 件", files.len());
            }
        }
        Ok(files)
    }

    /// ファイルを丸ごと受け取り、`sink` に流す。`progress(受信済み, 全体)` を呼ぶ。
    pub fn download(
        &mut self,
        f: &FileEntry,
        mut sink: impl FnMut(&[u8]) -> Result<()>,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<()> {
        self.buf.clear();
        self.send(CMD_TRANSFER_FILE, f.name.as_bytes())?;
        let total = f.length as u64;
        let mut got = 0u64;
        let result = (|| -> Result<()> {
            while got < total {
                let p = self
                    .recv(CMD_TRANSFER_FILE, Duration::from_secs(15))?
                    .ok_or_else(|| anyhow!("受信が止まった（{got}/{total} bytes）"))?;
                if p.body.is_empty() {
                    continue;
                }
                got += p.body.len() as u64;
                sink(&p.body)?;
                progress(got.min(total), total);
            }
            Ok(())
        })();
        if result.is_err() {
            self.flush();
        }
        result
    }
}

impl Drop for Jensen {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(INTERFACE);
    }
}

fn header_count(data: &[u8]) -> Option<usize> {
    (data.len() >= 6 && data[0] == 0xFF && data[1] == 0xFF)
        .then(|| u32::from_be_bytes(data[2..6].try_into().unwrap()) as usize)
}

/// 一覧の本体: [0xFFFF 件数u32]? { version u8 | 名前長 u24 | 名前 | 長さ u32 | 6 bytes | 署名 16 bytes }*
pub fn parse_file_list(data: &[u8]) -> Vec<FileEntry> {
    let mut off = 0;
    let expected = header_count(data);
    if expected.is_some() {
        off = 6;
    }
    let mut out = Vec::new();
    while off + 4 <= data.len() && expected.is_none_or(|n| out.len() < n) {

        let name_len = u32::from_be_bytes([0, data[off + 1], data[off + 2], data[off + 3]]) as usize;
        off += 4;
        if off + name_len + 4 + 6 + 16 > data.len() {
            break;
        }
        let name: String = data[off..off + name_len]
            .iter()
            .take_while(|&&b| b != 0)
            .filter(|b| b.is_ascii())
            .map(|&b| b as char)
            .collect();
        off += name_len;
        let length = u32::from_be_bytes(data[off..off + 4].try_into().unwrap());
        off += 4 + 6 + 16;
        out.push(FileEntry { name, length });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, len: u32) -> Vec<u8> {
        let mut v = vec![1u8];
        let n = name.len() as u32;
        v.extend_from_slice(&n.to_be_bytes()[1..]);
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(&len.to_be_bytes());
        v.extend_from_slice(&[0; 6 + 16]);
        v
    }

    #[test]
    fn parses_list_with_header() {
        let mut d = vec![0xFF, 0xFF, 0, 0, 0, 2];
        d.extend(entry("2026Nov07-183636-Rec43.hda", 37059852));
        d.extend(entry("2026Oct28-175240-Rec42.hda", 9305580));
        let f = parse_file_list(&d);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].name, "2026Nov07-183636-Rec43.hda");
        assert_eq!(f[1].length, 9305580);
    }

    #[test]
    fn stops_on_truncated_entry() {
        let mut d = vec![0xFF, 0xFF, 0, 0, 0, 2];
        d.extend(entry("a.hda", 10));
        let e = entry("b.hda", 20);
        d.extend(&e[..e.len() - 3]);
        assert_eq!(parse_file_list(&d).len(), 1);
    }
}
