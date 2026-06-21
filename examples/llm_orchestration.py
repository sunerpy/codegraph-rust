#!/usr/bin/env python3
"""codegraph -> LLM 代码分析编排示例 / orchestration example.

演示推荐的分工(README §"结合 LLM"的 ② 模式):
  - codegraph 出**确定性**结构事实(图谱 / 子图 / 中心性 / 影响面),毫秒级、可复现、零 LLM 成本;
  - 外部 LLM 只对**已定位的小上下文**做语义分析(诊断 / 摘要 / 建议),不啃整个仓库。

codegraph 二进制本身**不内置 LLM**(no-AI guardrail);LLM 调用发生在本脚本(编排层),
这正是 opsx 后端替换 graphify 的落地形态。

用法:
    # 1) 索引目标仓库(一次性)
    ./codegraph init /path/to/repo

    # 2) 跑编排(默认 dry-run,只打印将要发给 LLM 的 prompt,不真正调用)
    python examples/llm_orchestration.py --repo /path/to/repo --query "auth flow"

    # 3) 真正调用 LLM:设环境变量后去掉 --dry-run
    export OPENAI_API_KEY=sk-...                 # 或任意兼容 OpenAI 的端点
    export CODEGRAPH_LLM_BASE_URL=https://api.openai.com/v1   # 可选,默认 OpenAI
    export CODEGRAPH_LLM_MODEL=gpt-4o-mini                    # 可选
    python examples/llm_orchestration.py --repo /path/to/repo --query "auth flow" --call-llm

环境:仅依赖 Python 标准库(urllib);codegraph 二进制路径默认取仓库根的 ./codegraph,
可用 --bin 覆盖。
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path


def run_codegraph(bin_path: str, args: list[str]) -> str:
    """跑一条 codegraph 命令,返回 stdout。失败则带 stderr 抛出。"""
    env = {**os.environ, "CODEGRAPH_NO_DAEMON": "1"}
    proc = subprocess.run(
        [bin_path, *args],
        capture_output=True,
        text=True,
        env=env,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"codegraph {' '.join(args)} failed (exit {proc.returncode}):\n{proc.stderr}"
        )
    return proc.stdout


def top_god_nodes(bin_path: str, repo: str, limit: int) -> list[dict]:
    """导出全图(带中心性),返回 god_score 最高的若干符号节点。"""
    raw = run_codegraph(bin_path, ["export", "--path", repo])
    graph = json.loads(raw)
    code_nodes = [n for n in graph["nodes"] if n.get("file_type") == "code"]
    code_nodes.sort(key=lambda n: float(n.get("god_score") or 0.0), reverse=True)
    return code_nodes[:limit]


def explore(bin_path: str, repo: str, query: str) -> str:
    """用 explore 拿与 query 相关的跨文件子图 + 源码(给 LLM 的主上下文)。

    通过 MCP tools/call 调用 codegraph_explore;stdio 一发一收。
    """
    requests = (
        json.dumps({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}})
        + "\n"
        + json.dumps(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "codegraph_explore", "arguments": {"query": query}},
            }
        )
        + "\n"
    )
    env = {**os.environ, "CODEGRAPH_NO_DAEMON": "1"}
    proc = subprocess.run(
        [bin_path, "serve", "--mcp", "--path", repo],
        input=requests,
        capture_output=True,
        text=True,
        env=env,
    )
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("id") == 1 and "result" in msg:
            return msg["result"]["content"][0]["text"]
    return "(explore 无结果)"


def build_prompt(query: str, gods: list[dict], explore_text: str) -> str:
    """把 codegraph 的确定性事实组装成给 LLM 的分析 prompt。"""
    god_lines = "\n".join(
        f"- {n['label']} ({n['kind']}) — {n['source_file']}  "
        f"[god_score={float(n.get('god_score') or 0):.4f}, in={n.get('in_degree', 0)}, out={n.get('out_degree', 0)}]"
        for n in gods
    )
    return f"""你是一名资深代码分析师。下面是由 codegraph(确定性 tree-sitter 代码图谱)
提供的**精确结构事实**,请基于它们做分析——这些事实是可信的,不要臆测代码里没有的东西。

## 用户问题
{query}

## 架构中枢节点(按 PageRank 中心性,改动风险/理解优先级最高)
{god_lines}

## 与问题相关的跨文件流程 + 源码(codegraph explore)
{explore_text}

## 你的任务
1. 用 2-4 句话回答用户问题,引用上面出现的具体符号/文件。
2. 指出 1-3 个诊断要点(耦合热点、潜在影响面、可疑设计),关联到中枢节点或 explore 路径。
3. 若信息不足以下结论,明确说明还需要 codegraph 的哪个查询(callers/callees/impact/check)。
保持简洁,基于事实。"""


def call_llm(prompt: str) -> str:
    """调用兼容 OpenAI Chat Completions 的端点。仅用标准库。"""
    api_key = os.environ.get("OPENAI_API_KEY") or os.environ.get("CODEGRAPH_LLM_API_KEY")
    if not api_key:
        raise RuntimeError("未设置 OPENAI_API_KEY / CODEGRAPH_LLM_API_KEY")
    base_url = os.environ.get("CODEGRAPH_LLM_BASE_URL", "https://api.openai.com/v1").rstrip("/")
    model = os.environ.get("CODEGRAPH_LLM_MODEL", "gpt-4o-mini")
    body = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": prompt}],
            "temperature": 0,
        }
    ).encode()
    req = urllib.request.Request(
        f"{base_url}/chat/completions",
        data=body,
        headers={"Authorization": f"Bearer {api_key}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            payload = json.loads(resp.read())
    except urllib.error.HTTPError as exc:
        raise RuntimeError(f"LLM 调用失败 {exc.code}: {exc.read().decode(errors='replace')}") from exc
    return payload["choices"][0]["message"]["content"]


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description="codegraph -> LLM 代码分析编排示例")
    parser.add_argument("--repo", default=str(repo_root), help="目标仓库路径(默认:本项目)")
    parser.add_argument("--query", required=True, help="分析问题,例如 'how does indexing work'")
    parser.add_argument("--bin", default=str(repo_root / "codegraph"), help="codegraph 二进制路径")
    parser.add_argument("--gods", type=int, default=10, help="取前 N 个中枢节点")
    parser.add_argument("--call-llm", action="store_true", help="真正调用 LLM(否则只打印 prompt)")
    args = parser.parse_args()

    if not Path(args.bin).exists():
        print(f"找不到 codegraph 二进制:{args.bin}(先 cargo build --release)", file=sys.stderr)
        return 1
    if not (Path(args.repo) / ".codegraph" / "codegraph.db").is_file():
        print(f"仓库未索引:先跑 `{args.bin} init {args.repo}`", file=sys.stderr)
        return 1

    print("→ [1/3] 导出全图 + 中心性,取中枢节点 …", file=sys.stderr)
    gods = top_god_nodes(args.bin, args.repo, args.gods)
    print("→ [2/3] explore 相关子图 …", file=sys.stderr)
    explore_text = explore(args.bin, args.repo, args.query)
    print("→ [3/3] 组装 prompt …", file=sys.stderr)
    prompt = build_prompt(args.query, gods, explore_text)

    if not args.call_llm:
        print("\n===== DRY-RUN:以下是将发给 LLM 的 prompt(加 --call-llm 真正调用)=====\n")
        print(prompt)
        return 0

    print("→ 调用 LLM …", file=sys.stderr)
    answer = call_llm(prompt)
    print("\n===== LLM 分析结果 =====\n")
    print(answer)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
