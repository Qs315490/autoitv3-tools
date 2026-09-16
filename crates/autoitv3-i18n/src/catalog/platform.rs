//! Messages from `autoitv3-platform` (Windows emulation, native Windows layer,
//! file/RPC/DLL shims) and `autoitv3-unpack` (executable and script unpacking).

pub static ENTRIES: &[(&str, &str)] = &[
    // autoitv3-unpack — top-level error type.
    (
        "no packed payload found (expected a derived-key loader plus three stream-cipher members among the resources)",
        "未找到打包载荷（资源中应有一个派生密钥加载器和三个流密码成员）",
    ),
    (
        "no compiled script found (expected an AU3!EA05 or AU3!EA06 chunk, a resource named SCRIPT, or a raw chunk)",
        "未找到编译脚本（应为 AU3!EA05 或 AU3!EA06 数据块、名为 SCRIPT 的资源，或原始数据块）",
    ),
    (
        "the image is {packer}-packed: its script and resources are inside the packed data, so unpack the stub first (for example `upx -d FILE`) and read the result",
        "该镜像是 {packer} 加壳的：脚本和资源都在压缩数据里，先脱壳（例如 `upx -d FILE`）再读结果",
    ),
    (
        "the build carries no script entry (only embedded payloads)",
        "该文件不包含脚本条目（只有内嵌载荷）",
    ),
    ("the packed payload is malformed: {why}", "打包载荷格式错误：{why}"),
    ("invalid index spec: {why}", "无效的索引规格：{why}"),
    // autoitv3-unpack — payload decoding stages.
    ("the AES-192 password block has no padding", "AES-192 密码块没有填充"),
    ("the AES-128 layer has no padding", "AES-128 层没有填充"),
    ("the AES-256 layer has no padding", "AES-256 层没有填充"),
    ("block {n} fails its SHA-1 check", "第 {n} 块未通过 SHA-1 校验"),
    (
        "the payload is too short to carry a separator",
        "载荷太短，无法包含分隔符",
    ),
    (
        "a block is too short to carry its length marker",
        "数据块太短，无法包含长度标记",
    ),
    ("a block's length marker is not hex", "数据块的长度标记不是十六进制"),
    (
        "a block's length marker runs past its start",
        "数据块的长度标记超出了起始位置",
    ),
    ("a hash block is too short", "哈希块太短"),
    ("a hash block decodes to nothing", "哈希块解码结果为空"),
    ("a hash block is too short to rotate", "哈希块太短，无法轮转"),
    ("a hash block yields no password material", "哈希块未产生任何密码素材"),
    ("the AES-192 block has no length header", "AES-192 块没有长度头"),
    ("the password header is malformed", "密码头格式错误"),
    ("the password header is not a number", "密码头不是数字"),
    ("a hex run has an odd length", "十六进制串长度为奇数"),
    ("a hex run is not hexadecimal", "十六进制串包含非十六进制字符"),
    ("{part} is a descending range", "{part} 是降序范围"),
    ("{n} is past the end ({count} entries)", "{n} 超出末尾（共 {count} 项）"),
    ("{shown} is not a 1-based index", "{shown} 不是从 1 开始的索引"),
    // autoitv3-unpack — LZ decompressor.
    (
        "the compressed blob is shorter than its signature",
        "压缩数据短于其签名",
    ),
    (
        "the compressed blob is not {expected} (starts with {actual} )",
        "压缩数据的签名不是 {expected}（以 {actual} 开头）",
    ),
    ("the compressed blob has no size header", "压缩数据没有大小头"),
    (
        "the compressed blob claims {size} bytes, past the {cap} byte cap",
        "压缩数据声称有 {size} 字节，超过 {cap} 字节上限",
    ),
    (
        "a back-reference copies from before the output",
        "反向引用复制了输出之前的数据",
    ),
    ("the compressed stream ends mid-item", "压缩流在数据项中途结束"),
    // autoitv3-unpack — token deassembler.
    ("the token stream runs off the end", "标记流超出末尾"),
    (
        "a string of {key} characters runs off the end",
        "长度为 {key} 个字符的字符串超出末尾",
    ),
    ("a string is not valid UTF-16", "字符串不是有效的 UTF-16"),
    ("a keyword index is out of range", "关键字索引超出范围"),
    ("a function index is out of range", "函数索引超出范围"),
    ("unknown keyword {name}", "未知关键字 {name}"),
    ("unsupported opcode {opcode}", "不支持的操作码 {opcode}"),
    // autoitv3-unpack — script container.
    ("the token stream could not be read: {e}", "无法读取标记流：{e}"),
    (
        "the {record} record fails its Adler-32 check (want {checksum})",
        "记录 {record} 未通过 Adler-32 校验（期望 {checksum}）",
    ),
    (
        "the {record} record is compressed oddly: {e}",
        "记录 {record} 的压缩数据异常：{e}",
    ),
    ("no records follow the signature", "签名之后没有记录"),
    ("a record is longer than the chunk", "记录长度超过数据块"),
    ("the chunk ends in the middle of a record", "数据块在记录中途结束"),
    ("a UTF-16 string has an odd byte count", "UTF-16 字符串的字节数为奇数"),
    ("a UTF-16 string is malformed", "UTF-16 字符串格式错误"),
    // autoitv3-platform — PE resource reader.
    ("not a PE image (missing MZ)", "不是 PE 映像（缺少 MZ）"),
    ("truncated DOS header", "DOS 头被截断"),
    ("not a PE image (missing PE signature)", "不是 PE 映像（缺少 PE 签名）"),
    ("truncated COFF header", "COFF 头被截断"),
    ("truncated optional header", "可选头被截断"),
    ("unknown optional header magic {magic}", "未知的可选头标识 {magic}"),
    ("truncated data directories", "数据目录被截断"),
    ("resource directory RVA is not mapped", "资源目录 RVA 未映射"),
    // autoitv3-platform — DllStruct definition parser.
    ("DllStruct: bad alignment in {token}", "DllStruct：{token} 中的对齐值无效"),
    (
        "DllStruct: unknown type {type_name} in {definition}",
        "DllStruct：{definition} 中的类型 {type_name} 未知",
    ),
    (
        "DllStruct: {definition} declares no fields",
        "DllStruct：{definition} 未声明任何字段",
    ),
    // autoitv3-platform — emulated DllCall diagnostics.
    ("[winemu] DllCall not emulated: {dll}!{function}", "[winemu] DllCall 未模拟：{dll}!{function}"),
    ("[winemu] resource {name} from file", "[winemu] 资源 {name} 来自文件"),
    (
        "[winemu] CryptCreateHash: unsupported hash algid {algid}",
        "[winemu] CryptCreateHash：不支持的哈希算法 ID {algid}",
    ),
    (
        "[winemu] CryptDeriveKey: unsupported cipher algid {algid}",
        "[winemu] CryptDeriveKey：不支持的密码算法 ID {algid}",
    ),
    ("[winemu] cannot read module {path}: {e}", "[winemu] 无法读取模块 {path}：{e}"),
    // autoitv3-platform — native Windows COM layer.
    ("CoInitializeEx failed", "CoInitializeEx 失败"),
    ("empty ProgID", "ProgID 为空"),
    ("CLSIDFromProgID({name}) failed: {hr}", "CLSIDFromProgID({name}) 失败：{hr}"),
    ("CoCreateInstance({name}) failed: {hr}", "CoCreateInstance({name}) 失败：{hr}"),
    ("released object", "对象已释放"),
    // autoitv3-platform — native Windows elevation.
    (
        "ShellExecuteExW(runas) failed: {error}",
        "ShellExecuteExW(runas) 失败：{error}",
    ),
    // autoitv3-platform — native Windows resource diagnostics.
    (
        "[win32] GetModuleHandleW(NULL): cannot map resource image {path} (LoadLibraryExW failed)",
        "[win32] GetModuleHandleW(NULL)：无法映射资源映像 {path}（LoadLibraryExW 失败）",
    ),
    (
        "[win32] GetModuleHandleW(NULL) -> resource image {path} @ {base}",
        "[win32] GetModuleHandleW(NULL) -> 资源映像 {path} @ {base}",
    ),
    ("[win32] DllCall not resolved: {target}", "[win32] DllCall 无法解析：{target}"),
];
