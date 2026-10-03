# Quick start

This page indexes a small project and asks it the questions CodeGraph is for, with the output you should see.

You need CodeGraph installed ([Install](install.md)). Every command below takes the project with `-p`; run them in
your own repository the same way.

## A sample project

The examples use a four-file TypeScript shop. Create it in an empty directory to follow along:

::: details The four files

`src/cart.ts`

```typescript
export interface Item {
  name: string;
  price: number;
  quantity: number;
}

export class Cart {
  private items: Item[] = [];

  add(item: Item): void {
    this.items.push(item);
  }

  subtotal(): number {
    return this.items.reduce((sum, item) => sum + item.price * item.quantity, 0);
  }
}
```

`src/pricing.ts`

```typescript
const DISCOUNTS: Record<string, number> = { SPRING: 0.1, MEMBER: 0.05 };

export function applyDiscount(amount: number, code?: string): number {
  const rate = code ? (DISCOUNTS[code] ?? 0) : 0;
  return roundCents(amount * (1 - rate));
}

export function tax(amount: number): number {
  return roundCents(amount * 0.08);
}

function roundCents(value: number): number {
  return Math.round(value * 100) / 100;
}
```

`src/checkout.ts`

```typescript
import { Cart } from "./cart";
import { applyDiscount, tax } from "./pricing";

export function checkout(cart: Cart, code?: string): number {
  const discounted = applyDiscount(cart.subtotal(), code);
  return discounted + tax(discounted);
}
```

`src/main.ts`

```typescript
import { Cart } from "./cart";
import { checkout } from "./checkout";

const cart = new Cart();
cart.add({ name: "notebook", price: 4.5, quantity: 2 });
cart.add({ name: "pen", price: 1.2, quantity: 3 });
console.log(checkout(cart, "SPRING"));
```

:::

## 1. Build the index

```sh
cd shop
codegraph init .
```

```text
Scanning files…
Initialized in /tmp/shop
Indexed 4 files
22 nodes, 39 edges in 94ms
```

`init` creates `.codegraph/` in the project and indexes every file it recognises. Files ignored by `.gitignore`, and
dependency and build directories such as `node_modules` and `target`, are left out. Add `.codegraph/` to your
`.gitignore`: it is a local cache, rebuilt from the source at any time.

## 2. Check it

```sh
codegraph status .
```

```text
CodeGraph Status

Project: /tmp/shop

Index Statistics:
  Files:     4
  Nodes:     22
  Edges:     39
  DB Size:   0.16 MB
  Backend:   rusqlite - bundled SQLite
  Journal:   wal

  DB Path:   /tmp/shop/.codegraph/codegraph.db
  Daemon:    stopped

Nodes by Kind:
  class           1
  constant        2
  file            4
  function        4
  import          4
  interface       1
  method          2
  property        4

Files by Language:
  typescript      4

Index is up to date
```

`status --json` gives the same facts for scripts, including whether the index is current or which files changed
since it was built.

## 3. Read one symbol

```sh
codegraph node applyDiscount -p .
```

````text
## applyDiscount (function)

**Location:** src/pricing.ts:3
**Signature:** `(amount: number, code?: string): number`

```typescript
3	export function applyDiscount(amount: number, code?: string): number {
4	  const rate = code ? (DISCOUNTS[code] ?? 0) : 0;
5	  return roundCents(amount * (1 - rate));
6	}
```
### Trail — codegraph_node any of these to follow it (no Read needed)
**Calls →** roundCents (src/pricing.ts:12)
**Called by ←** checkout (src/checkout.ts:4), checkout.ts (src/checkout.ts:1)
````

`node` prints a symbol's source with what it calls and what calls it, so you can keep following the trail. Given a
file path instead, it prints the file with line numbers and the files that depend on it. Output is Markdown, because
the same text goes to coding agents.

## 4. Ask about an area

```sh
codegraph explore "how does checkout compute the total" -p .
```

````text
## Exploration: how does checkout compute the total

Found 10 symbols across 4 files.

### Blast radius — what depends on these (update/verify before editing)

- `checkout` (src/checkout.ts:4) — 1 caller in `src/main.ts`; no tests found within 3 caller hops

### Source Code

> The code below is the **verbatim, current on-disk source** of these files — re-read from disk on this call and line-numbered, byte-for-byte identical to what the Read tool returns. It is NOT a summary, outline, or stale cache. Treat each block as a Read you have already performed: do not Read a file shown here.

#### src/cart.ts — Cart(class), subtotal(method)

```typescript
1	export interface Item {
…
```
````

The output continues with the source of all four files. `explore` takes a question or a list of names, finds the
symbols involved, and returns their source grouped by file, what depends on them, and the calls between them. It is
the command line's view of `codegraph_explore`, the tool coding agents call most.

## 5. Follow calls and changes

| Command                                | Lists                                                                                                                      |
| -------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `codegraph search applyDiscount -p .`  | the symbols whose name matches, best match first, with their location and signature                                        |
| `codegraph callers applyDiscount -p .` | what calls or imports it: here `checkout` and the file `checkout.ts`                                                       |
| `codegraph callees checkout -p .`      | what it calls: `applyDiscount`, `subtotal`, `tax`, and the `Cart` type it uses                                             |
| `codegraph impact applyDiscount -p .`  | everything a change would reach, through callers of callers, grouped by file: here `checkout`, `checkout.ts` and `main.ts` |

Each of them takes `--json` for scripts.

## Next steps

- [Connect a coding agent](agents.md) so it can ask these questions itself.
- [Open the browser viewer](viewer.md) to read the same index visually.
- [Keep the index current](keeping-current.md) as you edit.
