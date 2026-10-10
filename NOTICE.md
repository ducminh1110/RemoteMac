# Notices

RemoteMac is free software: you can redistribute it and/or modify it under the terms of the
GNU General Public License as published by the Free Software Foundation, either version 3 of
the License, or (at your option) any later version. See [LICENSE](LICENSE).

It contains, or is derived from, code of these projects (all GPL-3.0 compatible):

| Project | License | Used for |
|---|---|---|
| [moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c) | GPL-3.0 | Windows client streaming core (vendored in `third_party/moonlight-common-c`) |
| [ENet (Moonlight fork)](https://github.com/cgutman/enet) | MIT | reliable UDP for control and input |
| [nanors](https://github.com/sleepybishop/nanors) | MIT | Reed-Solomon FEC |
| [moonlight-qt](https://github.com/moonlight-stream/moonlight-qt) | GPL-3.0 | D3D11 NV12 renderer approach and shaders |
| [Sunshine](https://github.com/LizardByte/Sunshine) | GPL-3.0 | Mac host: RTSP, control, video/FEC packetizing and input, ported |
| [MobileLab](https://github.com/ducminh1110/MobileLab) | MIT | the Liquid Glass material (`crates/rm-viewer/src/glass.rs`), ported from its Qt implementation; the colour tokens and tinted window of MacBridge's own windows (`crates/rm-viewer/src/look.rs`), after its interface design |
| [rustybuzz](https://github.com/harfbuzz/rustybuzz) | MIT | shaping MacBridge's own text (kerning, ligatures, marks) (`crates/rm-viewer/src/text.rs`) |
| [ab_glyph_rasterizer](https://github.com/alexheretic/ab-glyph) | Apache-2.0 | drawing that text's outlines (exact coverage) |
| [Inter](https://rsms.me/inter/) | OFL-1.1 | the typeface of MacBridge's own windows (bundled, `crates/rm-viewer/fonts`, licence beside it) |
| [JetBrains Mono](https://www.jetbrains.com/lp/mono/) | OFL-1.1 | technical details (bundled, licence beside it) |

## MobileLab (MIT)

`crates/rm-viewer/src/glass.rs` is a Rust port of MobileLab's Liquid Glass math and materials
(`linux-app/mobilelab-android/src/ui/glass/GlassMath.cpp`, `Glass.cpp`).

```
MIT License

Copyright (c) 2026 Minh

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
