# Everything SDK（随程序分发的部分）

这里只放了一个文件：

- `Everything64.dll` —— Everything 官方 SDK 里的 x64 动态库（91 KB），
  用来在本程序里按文件名查全盘路径（见 `src/everything.rs`）。

## 来源

Everything SDK（`Everything-SDK.zip`）由 voidtools 发布：
<https://www.voidtools.com/support/everything/sdk/>

从 zip 里取的就是 `dll/Everything64.dll`，未做任何修改。

## 为什么要带上它

本程序不再自己建全盘文件索引（那份索引要遍历整盘、常驻几十 MB 内存、还要定时重扫），
改成把「裸文件名」交给 Everything 查一次完整路径。Everything 本身是一个独立程序，
没装的时候我们也能跑（退回本地的可执行文件小索引），但把 DLL 一起放进来就意味着：

- 装了 / 开着 Everything 的机器：开箱即用，不用手动拷 DLL；
- 没装 Everything 的机器：DLL 加载成功但查询会返回「没有运行」，程序照常工作。

## 分发说明

`pack.bat` 会把这个 DLL 复制到 `dist\`，和 `RunAny_RS.exe` 放在一起。
如果你自己打发行包，记得带上它；不想带也能用，只是本地索引里没有的名字补不出完整路径。

Everything SDK 的使用条款见 voidtools 的 SDK 页面（允许随应用免费分发其 DLL）。
