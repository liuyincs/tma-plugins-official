# Compatibility

| Item | Value |
| --- | --- |
| Plugin ID | `tma.official.subsonic` |
| Plugin release | `0.1.0` |
| Host ABI | `1.6` |
| Subsonic protocol | `1.16.1` response envelope |
| Authentication | TMA plugin credential through `Authorization: Bearer`, `credential=`, `p=` (including `enc:<hex>`), OpenSubsonic `apiKey`, or the Subsonic `t`/`s` pair; `u` is required and must match the credential's user |
| Response formats | XML by default; JSON with `f=json` or an `Accept` header containing `json` |
| Streaming | Original bytes or TMA transcode profiles (`format`/`maxBitRate`) with HTTP Range support |
| Mutations | None |

Requests must declare Subsonic version `1.16.1`; unknown query parameters and
unknown endpoints return the standard failed response. `getArtists` honors the
standard `count` and `offset` paging parameters within the host catalog limit.

Implemented endpoints are `ping`, `getMusicFolders`, `getArtists`, `getAlbum`,
`getSong`, `getCoverArt`, and `stream` (both `/rest/name` and the standard
`/rest/name.view` spelling). Unsupported write, search, scrobble,
and playlist methods return a standard Subsonic failed response.

Authentication is owned entirely by the host: `t`/`s` is verified as
`md5(secret+salt)` against the user located by `u`, and authentication
parameters are stripped before requests reach the plugin. Transcoding is
delegated to the host's existing format registry and cache; the plugin never
reads files or invokes ffmpeg directly.
