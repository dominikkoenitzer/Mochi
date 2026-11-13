# Download page

The page behind https://mochi.punds.ch: static files, no build step on the server.
It names no author and does not link to this repository.

- `page.json` holds every word on the page and which screenshots it shows.
- `build.py` turns `page.json` into `index.html`. The same `build.py` sits in every app's
  `site/` folder, so all the download pages share one design. After an edit:

  ```
  python build.py
  ```

- `img/` holds real screenshots of the app as WebP, `-light` and `-dark` where the app has both
  looks. Never put a mock-up or an edited screenshot there.
- `jakarta.woff2` is the Plus Jakarta Sans typeface, served from the page itself;
  `jakarta-OFL.txt` is its licence.
- `og.png` is the 1200x630 card a chat app shows when someone shares the page. It is a picture of
  the page's own hero: rebuild it when the hero screenshot changes.
- `vercel.json` holds the download addresses, each a temporary redirect:
  - `/install.ps1`
  - `/latest`
  - `/download/windows`
  - `/download/:file`
  - `/download/:tag/:file`
  - `/schema.json`
- `llms.txt` describes the app for AI assistants.
- `skill/mochi/` is the assistant skill, and `skill/mochi.zip` is that folder zipped. Rebuild the zip
  after an edit, from this folder:

  ```
  python -c "import shutil; shutil.make_archive('skill/mochi', 'zip', 'skill', 'mochi')"
  ```

- `diag/` is the diagnostics page at /diag. Nothing links to it and search engines are told to leave it
  out. It measures the visitor's browser for ten seconds and shows the result; nothing is sent.
  `diag/vendor/` is the shared diagnostic panel, copied in by `test/sync-diag.mjs` and never edited
  here. CI fails when a copied file no longer matches its record in `test/diag-upstream.json`.
- `test/` checks the page in Chromium, Firefox and WebKit at 360, 390, 768, 1440 and 2560 px, light and
  dark: no console errors, no failed requests, no sideways scrolling, no serious accessibility faults,
  screenshots against the baselines in `test/snapshots/`, every link answering, and the download
  button sending each system to a release file whose name stays the same from release to release.
  `test/serve.mjs` serves this folder with the addresses from `vercel.json`, the way the host does.

  ```
  cd test
  npm ci
  npx playwright install chromium firefox webkit
  npx playwright test
  ```

  The baselines are taken in CI's container (`.github/workflows/site.yml`), so the fonts match from run
  to run. A new screenshot fails CI once; its picture is in the run's snapshots artifact.

Vercel deploys this folder whenever a push to main changes it. `.vercelignore` keeps this README,
`build.py`, `page.json` and `test/` off the site.
