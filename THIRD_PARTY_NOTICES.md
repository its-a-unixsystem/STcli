# Third-Party Notices

## SillyTavern

- Project: [SillyTavern](https://github.com/SillyTavern/SillyTavern)
- License: GNU Affero General Public License v3.0
- Use in STcli: behavioral compatibility research for the versioned `sillytavern-1.18-core` profile

Compatibility profile `sillytavern-1.18-core` is pinned to tag `1.18.0`, commit `51ad27fb86d39a3daca3adaa970375c9670c12df`. The checked-in profile records the upstream source paths used to derive its exact macro manifest.

## Nanobear

- Project: [cavecomputing/nanobear](https://github.com/cavecomputing/nanobear)
- Creator: cavecomputing
- License: Creative Commons Attribution 4.0 International
- Source: [`st/nanobear-v2.1-chat.json`](https://github.com/cavecomputing/nanobear/blob/a3aa566983e96f1f0f29718622390fca4baa1bd6/st/nanobear-v2.1-chat.json)
- Use in STcli: redistributable complex Chat Completion preset for provider-request oracle parity

`compat/external/nanobear-v2.1-chat.json` is an unmodified copy at commit `a3aa566983e96f1f0f29718622390fca4baa1bd6`. `compat/external/nanobear-v2.1-oracle.json` records the fixture provider requests and embeds its source, revision, license, and SillyTavern compatibility-reference provenance. The license text is available at <https://creativecommons.org/licenses/by/4.0/legalcode>.

## Matt Pocock engineering skills

- Project: [mattpocock/skills](https://github.com/mattpocock/skills)
- License: MIT
- Installed files: `.agents/skills/`, `.claude/skills/`, and `skills-lock.json`

```text
MIT License

Copyright (c) 2026 Matt Pocock

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
