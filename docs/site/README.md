# docs/site — the pages of firlab.app/codegraph

This directory holds the words and screenshots of the CodeGraph website: Chinese at `docs/site/`, English at
`docs/site/en/`, one file per page and the same path in both languages. The site itself (the VitePress
configuration, the theme, the components and the deployment) lives in [`sunerpy/firlab`](https://github.com/sunerpy/firlab)
under `codegraph/`. The site has no domain of its own: firlab.app's GitHub Pages deploy publishes it at
<https://firlab.app/codegraph/>.

This file is for maintainers and is not published.

## Paths

Site paths below are relative to `https://firlab.app/codegraph/`.

| Path here                                                                                                                                                          | Published at                                                                            |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------- |
| `index.md`, `en/index.md`                                                                                                                                          | `/`, `/en/` (home pages; the words are in their frontmatter, see "The home pages")      |
| `guide/`, `reference/faq.md`, `privacy.md`, `developers.md` and the same paths under `en/`                                                                         | the user guide                                                                          |
| `../cli.md`, `../mcp.md`, `../ui.md`, `../languages.md`, `../godot.md`, `../troubleshooting.md`                                                                    | `/en/reference/<name>`, as they are; `/reference/<name>` is a generated Chinese pointer |
| `../architecture.md`, `../data-model.md`, `../equivalence.md`, `../grammar-manifest.md`, `../embedded-extraction.md`, `../benchmark.md`, `../benchmark-results.md` | `/en/dev/<name>`, with a generated Chinese pointer at `/dev/<name>`                     |
| `public/`                                                                                                                                                          | the site root (`/codegraph-logo.svg`, `/screens/*.webp`)                                |
| `tools/`, this file                                                                                                                                                | not published                                                                           |

The canonical references stay English, as `docs/AGENTS.md` asks. The site publishes them unchanged and gives each a
Chinese page that points to the English one, because VitePress's language switch maps the current path onto the other
locale and would otherwise lead to a 404.

## How a change reaches the site

1. A pull request that touches these files or the references above runs `.github/workflows/docs-site.yml`. It checks
   out the public firlab repository, syncs this directory into it with firlab's
   `codegraph/scripts/sync-codegraph-docs.sh`, builds the site and runs firlab's `codegraph/scripts/check-dist.sh`.
   The step fails on any of these:
   - a dead link, or a page that exists in one language only;
   - an unknown component, a missing screenshot or a malformed home page;
   - a banned word, or a link that leaves `/codegraph/`.

   It reads no secret, so a pull request from a fork runs it too. It is advisory and not part of `CI Success`.

2. After the merge, `.github/workflows/publish-site.yml` runs the same sync script and commits the result to firlab's
   `main` as `docs(codegraph): sync from codegraph-rust@<sha>`. It needs the repository secret `FIRLAB_DOCS_TOKEN`
   (see "One-time setup").
3. firlab's `deploy.yml` builds the main site and this one, checks this one, puts it at `dist/codegraph/` and deploys
   firlab.app to GitHub Pages.

The footer of every page names the codegraph-rust commit its content came from.

## Preview

```bash
git clone https://github.com/sunerpy/firlab ../firlab    # once
../firlab/codegraph/scripts/sync-codegraph-docs.sh "$PWD"
cd ../firlab/codegraph && pnpm install --frozen-lockfile && pnpm dev    # http://localhost:5173/codegraph/
```

Run the sync again after each edit. It stops and names the problem when a page has no counterpart in the other
language, uses a component the site does not register, shows a screenshot that does not exist, or uses a word from the
lists below. A sync from an uncommitted tree marks the footer commit `-dirty`. Run `python3 scripts/docs-check.py` as
well: it checks every relative link and anchor here, the same way GitHub renders them.

## Writing

- **Both languages together.** The Chinese and English page have the same sections in the same order.
- **Relative `.md` links.** Link pages with paths such as `../guide/install.md`, and the canonical references by their
  real path, such as `../../cli.md` from a Chinese guide page or `../../../cli.md` from an English one. GitHub and
  `docs-check.py` follow them as written; the sync script rewrites them to the site's paths. Frontmatter links, which
  components render, use site paths (`/guide/install`, `/en/guide/install`).
- **AS-BUILT.** Every command, flag, default and output must match the code and the canonical reference. Output blocks
  are pasted from a real run, never written by hand. When the two disagree, fix the page.
- **No counts and no performance claims.** No language or tool totals and no latency, memory or throughput figures in
  prose; the sync rejects "sub-millisecond" and 亚毫秒. A screenshot or an output block may show numbers, because it
  shows one real run.
- **Plain written language.** Chinese pages use the register of Apple's and Microsoft's Chinese documentation, no
  colloquial words (还没、没能、免得、搭的、咋、啥), a space between Chinese and Latin text, headings without a full stop.
  English pages avoid "just", "gonna" and "stuff". The first sentence of a page says what the page helps with.
- **User pages name no internals:** no crate names and no `§`. `developers.md` is the exception.
- **Unreleased or preview features** are marked with `<StatusTag>` and never described as generally available.
- **No real secrets or private hosts**, in text or in a screenshot.

## Components

Pages may use these components and no others; the sync rejects any other tag.

| Component                                                                           | Use                                                       |
| ----------------------------------------------------------------------------------- | --------------------------------------------------------- |
| `<StatusTag status="available \| preview" />`                                       | release state, shown as text                              |
| `<ScreenFigure src dark? width height alt caption? />`                              | a screenshot; `dark` is the same screen in the dark theme |
| `<Badge>`                                                                           | VitePress's own badge                                     |
| `HomeIndex`, `HomeSteps`, `SplitBlock`, `HomePlatforms`, `HomePrivacy`, `HomeScope` | the home pages only; they render the `home:` frontmatter  |

## The home pages

Both home pages keep their words in frontmatter: VitePress's `hero:` (name, text, tagline, buttons) and a `home:`
block that the components render. The build checks `home:` against firlab's
`codegraph/src/.vitepress/theme/data/home-schema.ts` and fails on a missing or an unknown field.

| Key         | Holds                                                                                                |
| ----------- | ---------------------------------------------------------------------------------------------------- |
| `facts`     | the lines under the tagline: `term`, `text` (at least two)                                           |
| `visual`    | the hero capture: `desktop` with `light`, `dark`, `width`, `height`, `alt`                           |
| `index`     | the feature index: `title`, `intro`, `groups[].items[]` (`title`, `body`, `status`, `link`)          |
| `steps`     | `title`, `items[]` (`title`, `body`, optional `command`; at least two)                               |
| `tools`     | the agent split's table: `columns` (three), `rows[]` (`question`, `cli`, `mcp`), `caption`           |
| `shots`     | captures that `<SplitBlock proof="screen" shot="…">` names, keyed by `shot`                          |
| `languages` | the language split's table: `columns` (three), `rows[]` (`depth`, `extracted`, `members`), `caption` |
| `platforms` | `title`, `intro`, `columns`, `rows[]` (`name`, `status`, `cells`: one fewer than `columns`), `note`  |
| `privacy`   | `title`, `intro`, `sendsLabel`, `modes[]` (`name`, `sends`, `detail`)                                |
| `scope`     | what CodeGraph does not do: `title`, `items[]`                                                       |

`status` is `available` or `preview`.

## Screenshots

The screenshots are WebP files in `public/screens/`, named `viewer-<view>-<theme>.webp`, 1440 × 900 at device pixel
ratio 1. The viewer's interface is English only, so one set serves both languages. They come from the real viewer
reading this repository's own index:

```bash
cargo build --release -p codegraph-rs
CODEGRAPH_CHROME=/path/to/chrome docs/site/tools/capture-screens.sh target/release/codegraph
```

`tools/capture-screens.sh` does four things:

1. it extracts this repository at a fixed commit into a fresh `/tmp/codegraph-rust`, so no home path shows;
2. it writes the corpus's `.codegraph/config.toml`, which excludes the viewer bundle as `AGENTS.md` recommends;
3. it indexes the corpus and serves it with `codegraph ui` on `127.0.0.1:4791`;
4. it runs `tools/capture-screens.mjs`.

That script drives Chrome over its DevTools protocol and installs nothing. It opens each view as a fresh page in the
light and the dark theme, and waits until the view has finished loading. It stops without saving on:

- a console error, a failed request or a timeout;
- a view that does not show what it should;
- a symbol lookup that does not find exactly one match.

`CODEGRAPH_SCREENS_COMMIT` changes the corpus commit and `CODEGRAPH_SCREENS_PORT` the port.

Look at every image before committing it. Capture again when the viewer's text or layout changes, and update the
`width`/`height` in both home pages and in the `<ScreenFigure>` tags if the size changes.

## One-time setup

`publish-site.yml` needs `FIRLAB_DOCS_TOKEN`: a fine-grained personal access token for `sunerpy/firlab` only, with
Contents read and write and nothing else. GitHub has no API that creates one, so it is made by hand in the GitHub web
interface and stored with:

```sh
gh secret set FIRLAB_DOCS_TOKEN --repo sunerpy/codegraph-rust
```

Until it exists, `publish-site.yml` stops at its first step with an error that says so, and the site keeps the last
synced content.
