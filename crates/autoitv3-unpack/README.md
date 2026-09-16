# autoitv3-unpack — 编译脚本与资源载荷解包

**从编译产物取回载荷**：`AU3!EA05`/`EA06` 编译脚本解包（密钥流 + LZ + token 流反汇编）与资源载荷的 7 阶段解码，`au3 unpack` 的实现库。

> 组件地图与工作区总览见[根 README](../../README.md)。

## 目录

```text
    autoitv3-unpack/         # 库 crate——从编译产物里取回载荷（au3 unpack）
      src/lib.rs            资源打包载荷的 7 阶段解码 + 资源角色自动识别
                            （loader/member 靠 SHA1 校验确认，不做名字假设）
      src/script/           编译脚本（AU3!EA05 / AU3!EA06）的解包
        mod.rs              定位 chunk（SCRIPT 资源或签名扫描）+ 容器记录解析
        keys.rs             EA06 的 LAME / EA05 的 MT 密钥流
        lz.rs               AutoIt 自带的 LZ 解压
        tokens.rs           token 流 → `.au3` 源码（含缩进重建）
        symbols.rs          词表的唯一入口：re-export ast 的关键字、runtime 的函数/宏
                            （表本身不在这里，见上面两个 crate 的 vocab.rs）
      tests/unit/           各模块的单元测试（`#[path]` 回挂，见[根 README](../../README.md) 的「测试」）
        payload.rs          资源载荷：索引选择、AutoIt 语义助手、端到端样本
        script_container.rs 容器记录读写、载荷构造与校验
        script_keys.rs      密钥流对照参考向量
        script_lz.rs        LZ 解压的字面量/回引/截断边界
        script_tokens.rs    反汇编与浮点拼写
```

## 解包编译脚本（`au3 unpack --script`）

`aut2exe` 会把脚本本身——**源码**，先 token 化再压缩——塞进它生成的 exe 里。
这个 chunk 自带全部信息，`au3 unpack --script` 不运行程序就能把它读回来，
还原成可以直接 `au3 parse` 的 `.au3` 源码：

```bash
au3 unpack build.exe --script -o recovered.au3   # 从 PE 里取出编译进去的脚本
au3 unpack chunk.bin  --script                   # 裸 chunk（已 dump 出来的段/资源）也行
```

只认**签名**，不认文件名。两种格式：

| 格式 | 版本 | 名字编码 | 密钥流 |
| ---- | ---- | -------- | ------ |
| `AU3!EA06` | AutoIt v3.2.0+ | UTF-16 | LAME（17 字 lagged-Fibonacci，输出是拼出来的 double） |
| `AU3!EA05` | 更早的 v3 | 单字节 | 改过的 Mersenne Twister（tempering / seeding 都不是教科书版） |

`AutoIt3Wrapper` 的产物通常把整个 chunk 放在名为 `SCRIPT` 的 `RT_RCDATA` 资源里，
所以**先看这个资源，找不到再扫全镜像的签名**；两种来源最后走同一条解码链。
签名前面还有 16 字节每次构建都不同的盐，格式里用不到，按 `AU3!EAxx` 定位即可。

解码链自上而下：

1. **容器**：`FILE` 开头的记录序列（子类型、构建时的临时路径、压缩标志、大小、
   Adler-32、两个 FILETIME、密文），读到不是 `FILE` 就结束；
2. **密钥流**：每条记录的密文用常量种子 XOR 解开——字符串字段还会把自身长度加到
   种子上（`script/keys.rs`）；
3. **校验**：Adler-32 不符就判定"这不是要找的 chunk"，所以镜像里偶然撞见的签名
   不会冒充成功；
4. **解压**：`EA05`/`EA06` 头 + 大端长度 + LZ（字面量 / 15 位回引 / 变长匹配，
   回引可以重叠，用来编码重复串）；
5. **反汇编**：`>>>AUTOIT SCRIPT<<<` 记录是 token 流，按词表还原名字、按关键字重建
   缩进（token 之间用空格连接，这是格式的原始样子）。词表不在解包 crate 里：
   关键字表归 `autoitv3-ast::vocab`（词法分析器说了算），内置函数/宏表归
   `autoitv3-runtime::vocab`（运行时说了算），`script/symbols.rs` 只是它们的
   唯一入口——表是**语言**的，不是容器格式的，项目里只留一份。这三张表都是
   「位置即 id」的（`FUNCTIONS[0]` 必须是 `Abs`），因为它们同时要解码 token 流里
   的索引，顺序不能改。
   `>AUTOIT UNICODE SCRIPT<` / `>AUTOIT SCRIPT<` 则是明文源码。

**加壳的镜像会被认出来，但不会替你脱壳**：UPX 会把压缩过的段改名成 `UPX0`/`UPX1`，
`UPX!` magic 也在；`packed_with()` 用这两条（任一命中即可）判定，`--script` 与资源
载荷两条路都会直接报"该镜像是 UPX 加壳的，先脱壳（例如 `upx -d FILE`）再读结果"，
而不是含糊地说"没找到编译脚本"。MPRESS/Themida 之类没有这么整齐的标记，要加就得
各加各的特征。

`>>>AUTOIT NO CMDEXECUTE<<<` 是解释器自己跳过的占位记录、没有载荷，解码时跳过。
`FileInstall` 的载荷在同一容器的其它记录里，`--script` 会把它们列到 stderr
（子类型、路径、大小），但 stdout 只有脚本本身。

> **与参考实现的对应**：这条链是 MIT 许可的
> [AutoIt-Ripper](https://github.com/nazywam/AutoIt-Ripper) 的 Rust 移植
> （`autoit_unpack.py` / `mt.py` / `lame.py` / `decompress.py` / `opcodes.py`）。
> **逐字节对齐**是硬指标：对真实的编译产物，`au3 unpack --script` 的输出与参考
> 实现的 SHA-1 完全一致。
> 连浮点字面量的打印都照抄 Python 的 `repr`（定点/科学计数法的阈值、整数补 `.0`、
> 指数字号），否则一位数字的差别就会破坏对齐。

### 签名扫描为什么不会认错

镜像里可能有多处 `AU3!EA06`（样本里就有两处，另一处在无关资源的密文里）。
搜索的做法是"**每个候选都完整解析一遍**"，而记录链的每一步都带校验：错误候选
要么第一条记录就不是 `FILE`（解出来为空 → 判失败），要么 Adler-32 过不去，于是
自动落到真正的那个。反过来，一旦某个容器能一路解到脚本，校验也已经证明它确实是
编译器写的 chunk，而不是巧合。

## 解包资源载荷（`au3 unpack`）

有些编译产物把载荷加密后放进 4 个 `RT_RCDATA` 资源（一个 loader + 三个 member），
只有它自己的脚本能读回来。`au3 unpack` 直接按该格式解码，**不需要 `.au3`，
也不需要那个几 MB 的 `.exe`**：

```bash
au3 unpack ./staged/          # 目录：AutoIt3Wrapper 落盘的 __NAME / __Res64/NAME / __ResImage/_NAME
au3 unpack build.exe          # 或者直接给 PE，自动枚举它的 RT_RCDATA
au3 unpack build.exe --raw    # --raw 输出拼接后的整段文本，默认一行一条
au3 unpack build.exe --table  # --table 带 1-based 索引编号
au3 unpack build.exe --at 152,1263,3147-3149   # 只取这几项
```

四个资源的**角色是自动认出来的**：包里没有名字标签，所以把每个候选依次当 loader、
每个有序三元组当 member 试，只有当整条链跑通——包括最后一层的 `SHA1(明文) == Hash`
——才接受。这一步保证了搜索不会被巧合骗到（错组合过不了摘要校验），也意味着
**资源名每次构建随机变化都不影响**：

```
$ au3 unpack build.exe
unpacked: loader <name>, members <a>/<b>/<c> (17 resources considered)
  4144 entries, 347887 bytes
```

实现放在独立 crate `autoitv3-unpack`：它是一个**打包器**的格式，不是通用 AutoIt
能力，所以不塞进解释器。解码链复用 `autoitv3-platform` 的 CryptoAPI 仿真
（`CryptDeriveKey` 的 HMAC 式扩展）和 PE 解析。

### 当作字符串表的独立基准

同一份载荷往往**就是脚本自己的字符串表**：脚本里那张 `$table[n]` 是它解出来再按序号取的，
所以解包结果第 N 行 = `$table[N]`（`[0]` 是条数，不是表项）。`--table` / `--at` 就是把这件事
做得顺手一点：

```bash
au3 unpack build.exe --at 152,1263        # 表项 152、1263 的原始值
```

于是**怀疑解释器把表算错了**时有一条不依赖解释器的对照链：拿 exe 独立解出表项，
和断点里 `print $table[152]` 的值逐项比。两边一致 → 表没问题，往别处找；
不一致 → 就是求值/解密那一段的问题。这条链在排查"脚本自己解不动自己的数据"时特别有用
——能先把"我们算错了"这个可能性排除掉。

### 资源从哪里读

编译后的 AutoIt 脚本把加密的表放进 **PE 镜像的 `RT_RCDATA` 资源**，
`FindResourceW` / `LoadResource` 再从那里取。有两种来源，**先文件夹、后镜像**：

1. **已提取的资源文件**（优先）。`AutoIt3Wrapper_Res_File_Add` 会把每个内嵌资源
   摊在脚本旁边，文件名可以从资源名反推：
   `__NAME`、`__Res64/NAME`、`__ResImage/_NAME`，最后才试裸名 `NAME`。
   在脚本所在目录和当前工作目录里依次找，大小写不敏感（`FindResourceW` 本来就是）。
   于是**只要有从 exe 提取出来的资源，就不需要那个几 MB 的 exe**。
2. **PE 镜像**（回退）。`--resource-module <FILE>` → `AU3_RESOURCE_MODULE` →
   自动查找：脚本所在目录、再当前工作目录，优先选与脚本同名的镜像，否则选第一个
   真正带资源的 PE（按文件名排序，保证可复现）。自动选中时会往 stderr 打一行
   `# resource module: ...` 提示。

两边都找不到时，`GetModuleHandleW`/`FindResourceW` 落回"未列举"分支
（`@error = 1`、返回 `0`），脚本自己决定怎么办 —— 边界可见，不编造数据。
`AU3_WINEMU_TRACE=1` 会打印每次"资源来自文件"的命中，便于确认读的是哪一份。
