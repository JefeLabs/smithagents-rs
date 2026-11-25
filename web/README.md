# SmithAgents web

Chat web app (Vite, React 19, TypeScript, Tailwind v4) shown in the desktop app's center tabs.

```
npm install
npm run dev         # dev server
npm test            # vitest (jsdom); does not build
npm run typecheck
npm run build       # outputs dist/
npm run test:build  # build, then assert dist/index.html and built CSS
npm run lint:colors # cargo themegen lint (no literal colors in web source)
```

## Theme

The theme files are generated into `../design/generated/` and consumed in place, never copied:

- `src/styles.css` imports `design/generated/theme.css` (Tailwind tokens such as `bg-background`).
- `vite.config.ts` inlines `design/generated/theme-bootstrap.js` as the first `<script>` in `<head>` (dev and build), so `data-theme`, `data-font` and `--text-scale` are set before styles paint. The host may set `window.__SMITH_THEME__` first.

Use only token-backed utilities; no literal colors. Add shadcn components with `npx shadcn@latest add <name>` (do not run `shadcn init`) and review the diff.

## Content-Security-Policy

The built `dist/index.html` carries a CSP `<meta>` (charset, CSP, then the inlined theme bootstrap, whose sha256 is computed in `vite.config.ts`). It is not applied in dev. The `'self'` sources (script, style, img, font) must be re-verified under the scheme the desktop host finally uses to load the page (file:// vs a custom scheme) in plan Task 16.
