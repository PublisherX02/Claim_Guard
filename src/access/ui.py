"""The reviewer console: a small single-page app served by the API itself (same origin, so the session cookie stays httpOnly and
SameSite=Strict and no cross-origin rule is needed).

The files in src/access/ui/ are read once at start-up into a fixed table, so a request can only ever name a file in that table:
there is no path to join and nothing to traverse. They carry no secret and no per-user data; everything a person sees comes from
the API after sign-in, under the same permission checks as any other client. The page loads its one script and one stylesheet from
its own origin, so its Content-Security-Policy allows scripts and styles from 'self' and nothing else (no inline script, no eval, no
framing). Every other route keeps the strict default-src 'none'.
"""
from pathlib import Path

from fastapi import Request
from fastapi.responses import RedirectResponse, Response

UI_DIR = Path(__file__).resolve().parent / 'ui'
UI_PREFIX = '/ui/'
UI_CSP = ("default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; "
          "base-uri 'none'; form-action 'none'; frame-ancestors 'none'")
TYPES = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8',
         '.svg': 'image/svg+xml', '.png': 'image/png', '.ico': 'image/x-icon'}


def load_files(directory=UI_DIR):
    """{name: (bytes, content type)} for every regular file under the UI directory, names relative and with forward slashes."""
    files = {}
    for path in sorted(Path(directory).rglob('*')):
        if path.is_file() and path.suffix in TYPES:
            files[path.relative_to(directory).as_posix()] = (path.read_bytes(), TYPES[path.suffix])
    return files


def install(app, directory=UI_DIR):
    files = load_files(directory)

    @app.get('/', include_in_schema=False)
    def root():
        return RedirectResponse('/ui/', status_code=307)

    @app.get('/ui', include_in_schema=False)
    def bare():
        return RedirectResponse('/ui/', status_code=307)

    @app.get('/ui/{name:path}', include_in_schema=False)
    def serve(name: str, request: Request):
        entry = files.get(name or 'index.html')
        if entry is None:
            return Response(b'{"error": "not_found"}', status_code=404, media_type='application/json')
        body, content_type = entry
        return Response(body, media_type=content_type)
