#!/usr/bin/env python3
"""Magpie CCursor Bridge — 前台 subagent 修复补丁

问题:Cursor 3.26.x 下前台 subagent 报
  [invalid_argument] Cursor user message action has no message_id

根因:引擎在 compile::run::action 里要求 action 必须带非空 message_id。
Cursor 通过 BYOK 桥发起的前台 subagent action 不带该字段(只有后台 subagent
走另一条不经过此校验的路径),于是被拒绝。

修法:把非空校验分支改成跳进引擎自带的"空 message 降级路径"
(原有代码,本来就为 message 列表为空的情况准备),而不是直接报错。

patch 点: 0x10076f348  cbz x8, 0x10076f738  ->  b 0x10076f780

仅适用于 engine_version 0.2.1 / macOS arm64。
"""
import sys, struct, hashlib

KNOWN_SHA = "c30b83c88a926d06ae85c4ccc46c17e3bd371c88cb4f879321328882474c0aee"
VM_BASE   = 0x100000000
PATCH_AT  = 0x10076f348
ORIGINAL  = 0xb4001f88          # cbz x8, 0x10076f738
PATCHED   = 0x1400010e          # b   0x10076f780

def main(src, dst):
    data = bytearray(open(src, "rb").read())
    sha = hashlib.sha256(data).hexdigest()
    if sha != KNOWN_SHA:
        print(f"  警告: 引擎 hash 不匹配 ({sha[:16]}…),可能不是 0.2.1")
    off = PATCH_AT - VM_BASE
    cur = struct.unpack_from("<I", data, off)[0]
    if cur == PATCHED:
        print("  已经是打过补丁的版本"); return 0
    if cur != ORIGINAL:
        print(f"  错误: 0x{PATCH_AT:x} 处指令是 0x{cur:08x},不符合预期"); return 1
    struct.pack_into("<I", data, off, PATCHED)
    open(dst, "wb").write(data)
    print(f"  已补丁: 0x{PATCH_AT:x}  0x{ORIGINAL:08x} -> 0x{PATCHED:08x}")
    return 0

if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("用法: patch-engine.py <原引擎> <输出>"); sys.exit(2)
    sys.exit(main(sys.argv[1], sys.argv[2]))
