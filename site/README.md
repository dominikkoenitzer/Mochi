# Download page

The page behind https://get-mochi.vercel.app: plain static files, no build step.
It names no author and does not link to this repository.

- `index.html` is the page.
- `vercel.json` holds the download addresses, each a temporary redirect:
  - `/install.ps1`: the newest release's `install.ps1`
  - `/latest`: the newest release's details (JSON)
  - `/download/windows`: the newest release's `Mochi-Setup.exe`
  - `/download/:file`: the newest release's `:file`
  - `/download/:tag/:file`: that file of that release
  - `/schema.json`: `schema.json` on the main branch
- `llms.txt` describes Mochi for AI assistants.
- `skill/mochi/` is the assistant skill, and `skill/mochi.zip` is that folder zipped. Rebuild the zip after an edit, from this folder:

  ```
  python -c "import shutil; shutil.make_archive('skill/mochi', 'zip', 'skill', 'mochi')"
  ```

Vercel deploys this folder whenever a push to main changes it. `.vercelignore` keeps this README off the site.
