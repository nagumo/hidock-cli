//! hidock — HiDock の録音を USB から一覧・ダウンロードする（読み取り専用）。
//!
//!   hidock info
//!   hidock list
//!   hidock pull <DIR> [--since YYYY-MM-DD] [--keep-ext]
//!
//! .hda は中身が MP3 なので、保存時に拡張子を .mp3 に変える（--keep-ext で元のまま）。

mod jensen;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use jensen::{FileEntry, Jensen};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "hidock", version, about = "HiDock の録音を USB から一覧・ダウンロードする（読み取り専用）")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 機種・ファームウェア・シリアル番号
    Info,
    /// 録音の一覧
    List,
    /// 録音をダウンロード（同じ名前・同じサイズのファイルがあれば飛ばす）
    Pull {
        /// 保存先のフォルダ
        dir: PathBuf,
        /// 落とす録音（ファイル名・拡張子なし・.mp3 名・名前の一部 例: Rec43, 2026Nov07）。省略で全部
        names: Vec<String>,
        /// この日以降の録音だけ（YYYY-MM-DD）
        #[arg(long)]
        since: Option<String>,
        /// 拡張子を .hda のまま保存する
        #[arg(long)]
        keep_ext: bool,
    },
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// "2026Nov07-183636-Rec43.hda" → ("2026-11-07", "18:36:36")
fn parse_name_datetime(name: &str) -> Option<(String, String)> {
    let b = name.as_bytes();
    if b.len() < 16 || b[9] != b'-' {
        return None;
    }
    let year = &name[0..4];
    let month = MONTHS.iter().position(|m| *m == &name[4..7])? + 1;
    let day = &name[7..9];
    let t = &name[10..16];
    if !(year.chars().all(|c| c.is_ascii_digit()) && t.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    Some((format!("{year}-{month:02}-{day}"), format!("{}:{}:{}", &t[0..2], &t[2..4], &t[4..6])))
}

fn output_name(name: &str, keep_ext: bool) -> String {
    match name.strip_suffix(".hda") {
        Some(stem) if !keep_ext => format!("{stem}.mp3"),
        _ => name.to_string(),
    }
}

/// 録音は 96kbps の MP3 なので、バイト数から長さを見積もる。
fn approx_minutes(f: &FileEntry) -> f64 {
    f.length as f64 / 12_000.0 / 60.0
}

fn sorted_files(dev: &mut Jensen) -> Result<Vec<FileEntry>> {
    let mut files = dev.list_files()?;
    files.sort_by_key(|f| parse_name_datetime(&f.name).unwrap_or_default());
    Ok(files)
}

fn cmd_list(dev: &mut Jensen) -> Result<()> {
    let files = sorted_files(dev)?;
    let mut total = 0u64;
    for f in &files {
        total += f.length as u64;
        let (d, t) = parse_name_datetime(&f.name).unwrap_or(("?".into(), "?".into()));
        println!("{d} {t}  {:6.1}分  {:7.1}MB  {}", approx_minutes(f), f.length as f64 / 1e6, f.name);
    }
    println!("--- {} 件 / {:.2} GB", files.len(), total as f64 / 1e9);
    Ok(())
}

/// 指定がファイルに当たるか: 完全一致・拡張子なし・保存名(.mp3)・名前の一部
fn name_matches(f: &FileEntry, pat: &str) -> bool {
    let stem = f.name.rsplit_once('.').map_or(f.name.as_str(), |(s, _)| s);
    let pat_stem = pat.rsplit_once('.').map_or(pat, |(s, _)| s);
    f.name == pat || stem == pat_stem || f.name.contains(pat)
}

/// 名前の指定で絞る。どれにも当たらない指定があればダウンロード前にエラーにする。
fn select_by_names(files: Vec<FileEntry>, names: &[String]) -> Result<Vec<FileEntry>> {
    if names.is_empty() {
        return Ok(files);
    }
    let missing: Vec<_> = names.iter().filter(|p| !files.iter().any(|f| name_matches(f, p))).collect();
    if !missing.is_empty() {
        bail!("見つからない: {}（hidock list で名前を確認）", missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    }
    Ok(files.into_iter().filter(|f| names.iter().any(|p| name_matches(f, p))).collect())
}

fn cmd_pull(dev: &mut Jensen, dir: &Path, names: &[String], since: Option<&str>, keep_ext: bool) -> Result<()> {
    if let Some(s) = since {
        if s.len() != 10 || s.as_bytes()[4] != b'-' || s.as_bytes()[7] != b'-' {
            bail!("--since は YYYY-MM-DD で指定する");
        }
    }
    let files: Vec<_> = sorted_files(dev)?
        .into_iter()
        .filter(|f| match (since, parse_name_datetime(&f.name)) {
            (Some(s), Some((d, _))) => d.as_str() >= s,
            _ => true,
        })
        .collect();
    let files = select_by_names(files, names)?;
    fs::create_dir_all(dir)?;
    let (mut ok, mut skipped, mut failed) = (0, 0, 0);
    for (i, f) in files.iter().enumerate() {
        let tag = format!("[{}/{}]", i + 1, files.len());
        let dest = dir.join(output_name(&f.name, keep_ext));
        if fs::metadata(&dest).is_ok_and(|m| m.len() == f.length as u64) {
            println!("{tag} skip（既にある） {}", dest.display());
            skipped += 1;
            continue;
        }
        let part = dest.with_extension(format!(
            "{}.part",
            dest.extension().and_then(|e| e.to_str()).unwrap_or("")
        ));
        let mut w = BufWriter::new(File::create(&part)?);
        let name = f.name.clone();
        let res = dev.download(
            f,
            |chunk| Ok(w.write_all(chunk)?),
            |got, total| {
                eprint!("\r{tag} {name} {:3}%", got * 100 / total.max(1));
            },
        );
        eprintln!();
        let res = res.and_then(|_| Ok(w.flush()?));
        drop(w);
        match res {
            Ok(()) if fs::metadata(&part)?.len() == f.length as u64 => {
                fs::rename(&part, &dest)?;
                println!("{tag} OK {}", dest.display());
                ok += 1;
            }
            Ok(()) => {
                println!("{tag} 失敗: サイズが合わない。{} を残した", part.display());
                failed += 1;
            }
            Err(e) => {
                println!("{tag} 失敗: {e}。{} を残した", part.display());
                failed += 1;
            }
        }
    }
    println!("--- 保存 {ok} / 既にある {skipped} / 失敗 {failed}");
    if failed > 0 {
        bail!("{failed} 件失敗した");
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut dev = Jensen::open()?;
    match cli.cmd {
        Cmd::Info => {
            let info = dev.device_info()?;
            println!("model:    HiDock {} (PID 0x{:04X})", dev.model, dev.pid);
            println!("firmware: {}", info.version);
            println!("serial:   {}", info.serial);
        }
        Cmd::List => {
            dev.device_info()?;
            cmd_list(&mut dev)?;
        }
        Cmd::Pull { dir, names, since, keep_ext } => {
            dev.device_info()?;
            cmd_pull(&mut dev, &dir, &names, since.as_deref(), keep_ext)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_datetime() {
        assert_eq!(
            parse_name_datetime("2026Nov07-183636-Rec43.hda"),
            Some(("2026-11-07".into(), "18:36:36".into()))
        );
        assert_eq!(parse_name_datetime("bad.hda"), None);
    }

    #[test]
    fn rename_ext() {
        assert_eq!(output_name("a-Rec1.hda", false), "a-Rec1.mp3");
        assert_eq!(output_name("a-Rec1.hda", true), "a-Rec1.hda");
        assert_eq!(output_name("a-Rec1.mp3", false), "a-Rec1.mp3");
    }
}

#[cfg(test)]
mod select_tests {
    use super::*;

    fn f(n: &str) -> FileEntry {
        FileEntry { name: n.into(), length: 1 }
    }

    #[test]
    fn matches_various_forms() {
        let a = f("2026Nov07-183636-Rec43.hda");
        assert!(name_matches(&a, "2026Nov07-183636-Rec43.hda"));
        assert!(name_matches(&a, "2026Nov07-183636-Rec43"));
        assert!(name_matches(&a, "2026Nov07-183636-Rec43.mp3"));
        assert!(name_matches(&a, "Rec43"));
        assert!(name_matches(&a, "2026Nov07"));
        assert!(!name_matches(&a, "Rec42"));
    }

    #[test]
    fn missing_name_is_error() {
        let files = vec![f("2026Nov07-183636-Rec43.hda"), f("2026Oct28-175240-Rec42.hda")];
        assert_eq!(select_by_names(files.clone(), &["Rec42".into()]).unwrap().len(), 1);
        assert_eq!(select_by_names(files.clone(), &[]).unwrap().len(), 2);
        assert!(select_by_names(files, &["Rec99".into()]).is_err());
    }
}
