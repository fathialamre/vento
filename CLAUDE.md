# Project rules

## Git commits

- Always commit and push as user `fathialamre` only.
- Never add `Co-Authored-By: Claude` (or any Claude attribution) to commit messages.
- Never set Claude as author or co-author.
- Do not include "Generated with Claude Code" footers in commit messages or PR bodies.
- Commit messages must read as if written by the user.

## UI: tabular editors (params, env vars, headers, etc.)

All key/value editor grids in Vento share the same spreadsheet-style look. When
building or modifying one, match this pattern:

- Outer wrapper: `overflow-hidden rounded-md border bg-card`.
- CSS grid (`grid-cols-[...]`) with the same column template on header and rows.
- Header row:
  - `sticky top-0 z-10`, `bg-muted/60`, `backdrop-blur`, `border-b`,
    `text-xs font-medium text-muted-foreground`.
  - Each header cell except the last gets `border-r`.
- Body row:
  - `border-b border-border/60 last:border-b-0`, hover with `hover:bg-accent/20`,
    `group/row` so the trailing delete icon can opacity-fade in on hover.
  - Each body cell except the last gets `border-r border-border/60`.
  - Inputs inside cells use `h-8 rounded-none border-0 bg-transparent font-mono
    text-xs shadow-none focus-visible:bg-background focus-visible:ring-0` so the
    grid lines stay visible.
- Trailing blank "draft" row: keep React identity stable by storing the draft id
  in `useRef` and rotating it only when the row is materialized — see
  `src/components/params-editor.tsx` (`draftIdRef`) and
  `src/components/environment-editor.tsx`. Do NOT key the blank row by a static
  string like `"blank"`; it loses focus on first keystroke.
