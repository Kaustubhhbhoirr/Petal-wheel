# Architecture

See SPEC.md section 6 for the full design. This file records decisions as they are made during the spike.

## Worker protocol

The core starts a worker, writes one JSON request to its stdin, closes stdin, and reads one JSON response
from stdout. Diagnostics go to stderr. Exit code 0 means the response was written.

Request:

```json
{ "op": "merge", "inputs": ["C:\a.pdf", "C:\b.pdf"], "output": "C:\a - merged.pdf.flick-tmp" }
```

Response:

```json
{ "ok": true, "output": "C:\a - merged.pdf.flick-tmp", "pages": 12 }
{ "ok": false, "error": { "code": "encrypted", "message": "b.pdf is password protected." } }
```

The worker writes only to the `output` path it was given. The core owns the final name (collision rule
`name (1).ext`) and renames the temp file only after a successful response.
