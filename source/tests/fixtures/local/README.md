# Local audio test fixtures

- `spec_mp3_cbr128.mp3`: ffmpeg-built CBR 128 kb/s MP3, copied verbatim from
  the musid-dli repository (`tests/fixtures/local/spec_mp3_cbr128.mp3`,
  the same committed fixture musid's `local_cover`/`tag_write`/`local` test
  suites embed). Used by `tests/musid_local_cover_parity.rs` for the
  embedded-cover cases: WAV has no cover slot (musid local_cover.rs test
  comment), so embedded-cover tests need an ID3v2-capable container.
