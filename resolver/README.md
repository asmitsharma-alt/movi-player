# Movies preview

The private Movies page and its API run in the same AppSail container. The main Movi Player site can link to it by setting the Worker variable `MOVIES_PREVIEW_URL` to the preview origin.

Build the Linux image from the repository root.

```sh
docker build --platform linux/amd64 -f Dockerfile.movies -t movi-movie-resolver:preview .
```

Set these variables on the AppSail service before activating the image.

| Variable | Value |
| --- | --- |
| `MOVI_ORIGIN` | Exact HTTPS origin of the deployed preview, without a trailing slash |
| `MOVI_TOKEN_KEY` | 32 random bytes encoded as unpadded base64url |
| `MOVI_ACCOUNTS` | JSON mapping of invited usernames to Argon2id password hashes |
| `X_ZOHO_CATALYST_LISTEN_PORT` | Port injected by AppSail |

Generate each password hash by passing a password to the resolver binary's `hash-password` command on standard input. Store passwords and signing keys outside source control. Keep the signing key stable between instances.

The public health endpoint is `/health`. Signed-in viewers enter at `/movies`. Successful catalog and playback API calls report resolver time in `Server-Timing`. Chromium exposes Play-to-first-frame samples in the page's Performance entries with the `movi:play-to-first-frame:` prefix.

The catalog parsing and mirror extraction derive from MovieBox-TUI commit `b901738b56170c278b35468c32ade32b9de1890d`, used under its Apache-2.0 license. [Upstream source and license](https://github.com/mesamirh/MovieBox-Tui/tree/b901738b56170c278b35468c32ade32b9de1890d)
