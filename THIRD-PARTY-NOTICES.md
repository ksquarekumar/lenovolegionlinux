# Third-party notices

Code that legionctl took from other projects, with their licenses.

## clauth 0.17.0

- Source: https://github.com/uwuclxdy/clauth (crate `clauth` 0.17.0).
- Taken: the ISO 8601 time parser (`iso_to_epoch_secs`), ported to `crates/legionctl-core/src/accounts.rs`. clauth
  itself is a runtime dependency (the account engine, todo-188): legionctl runs the `clauth` program and reads its
  files and status feed; no other code of it is in legionctl.

```text
MIT License

Copyright (c) 2026 cloudy

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
