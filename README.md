# hidock-cli

HiDock（H1 / H1E / P1 / P1 mini）の録音を、HiNotes を使わずに USB から一覧・ダウンロードする読み取り専用の CLI。

> **⚠️ 非公式ツールです。** HiDock の開発元・販売元とは一切関係がなく、承認・サポートも受けていません。
> USB プロトコルは公開されておらず、第三者の解析結果をもとにしています。ファームウェアの更新などで動かなくなることがあります。
> **データの消失・機器の故障・メーカー保証が受けられなくなることを含め、本ツールの使用によって生じたいかなる損害についても作者は責任を負いません。すべて自己責任で使ってください。**
> 大事な録音は、使う前に公式アプリ（HiNotes）でも保存しておくことをおすすめします。

## 動作確認した環境

- macOS 27（Apple Silicon）＋ HiDock P1（ファームウェア 1.4.5）だけ
- Windows・Linux や、ほかの機種（H1 / H1E / P1 mini）では試していません。コード上は対応する USB ID を入れてありますが、動く保証はありません
  - Linux では USB への権限（udev ルール）が、Windows では WinUSB ドライバが必要になるはずです

```
hidock info                                  # 機種・ファームウェア・シリアル
hidock list                                  # 録音の一覧
hidock pull ~/Downloads/hidock               # 全部（同じ名前・サイズがあれば飛ばす）
hidock pull ~/Downloads/hidock --since 2026-11-01
hidock pull ~/Downloads/hidock Rec43 Rec42           # 名前で指定（フル名・.mp3 名・名前の一部でOK）
hidock pull DIR --keep-ext                   # 拡張子を .hda のまま
```

- `.hda` は中身が MP3（96kbps・48kHz・モノラル）なので、保存時に `.mp3` に変える（再エンコードなし）
- 削除・フォーマットなどの書き込み系コマンドは実装していない
- Chrome（HiNotes）が HiDock を掴んでいると `インターフェースを確保できない` になる。Chrome を ⌘Q で完全に終了する
- `HIDOCK_DEBUG=1` で機器情報の生データを表示

プロトコルは [sgeraldes/hidock-light](https://github.com/sgeraldes/hidock-light)（MIT）の `hidock_device.py` を参考にした。

## ビルド

```
cargo build --release && cp target/release/hidock ~/.local/bin/
```
libusb は `rusb` の `vendored` で静的に入るので、brew の libusb は不要。

## ライセンス

MIT（[LICENSE](LICENSE)）。本ソフトウェアは「現状のまま」提供され、いかなる保証もありません。
HiDock は各権利者の商標です。
