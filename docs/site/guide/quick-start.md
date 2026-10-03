# 快速开始

本页为一个小项目建立索引，演示 CodeGraph 擅长回答的问题，并给出你应当看到的输出。

开始前请先安装 CodeGraph（见[安装](install.md)）。下面每条命令都用 `-p` 指定项目，在你自己的仓库中用法相同。

## 示例项目

示例使用一个由四个文件组成的 TypeScript 小商店。如需跟着操作，请在一个空目录中创建这些文件：

::: details 四个文件

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

## 1. 建立索引

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

`init` 在项目中创建 `.codegraph/`，并索引它能识别的所有文件。`.gitignore` 忽略的文件，以及 `node_modules`、`target` 等依赖和构建目录，不会被索引。请把 `.codegraph/` 加入 `.gitignore`：它是本地缓存，随时可以从源码重建。

## 2. 检查索引

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

`status --json` 以脚本可读的形式给出同样的信息，包括索引是否最新，以及建立索引后有哪些文件发生了变化。

## 3. 查看一个符号

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

`node` 打印符号的源码，以及它调用了什么、被什么调用，方便继续沿着调用链查看。传入文件路径时，它会打印带行号的文件内容和依赖该文件的其他文件。输出是 Markdown 格式，因为同样的文本也会提供给编码 Agent。

## 4. 了解一块代码

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

输出接着列出四个文件的源码。`explore` 接受一个问题或几个名称，找出涉及的符号，按文件返回它们的源码、依赖它们的代码，以及它们之间的调用。它是 `codegraph_explore` 在命令行中的对应命令，而 `codegraph_explore` 是编码 Agent 调用最多的工具。

## 5. 跟踪调用和改动

| 命令                                   | 列出                                                                                                        |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `codegraph search applyDiscount -p .`  | 名称匹配的符号，最匹配的排在最前，附带位置和签名                                                            |
| `codegraph callers applyDiscount -p .` | 调用或导入它的代码：这里是 `checkout` 和文件 `checkout.ts`                                                  |
| `codegraph callees checkout -p .`      | 它调用的代码：`applyDiscount`、`subtotal`、`tax`，以及它用到的类型 `Cart`                                   |
| `codegraph impact applyDiscount -p .`  | 改动它会影响的全部代码，沿调用方的调用方逐层展开，按文件分组：这里是 `checkout`、`checkout.ts` 和 `main.ts` |

这些命令都支持 `--json`，供脚本使用。

## 下一步

- [接入编码 Agent](agents.md)，让它自己提出这些问题。
- [打开浏览器查看器](viewer.md)，以可视化方式阅读同一份索引。
- [保持索引最新](keeping-current.md)，在编辑代码时跟上变化。
