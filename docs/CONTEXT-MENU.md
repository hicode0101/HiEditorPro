# 右键菜单集成：编译、注册与卸载

> 本文说明「用 HiEditor Pro 打开」右键菜单的两套机制——经典菜单的传统动词与
> Win11 新版菜单的稀疏 MSIX 包——各自的编译产物、注册流程、删除注册步骤，以及
> 常见错误码排查。用户视角的快速使用说明见 [README](../README.md) 右键菜单章节。

## 0. 两种机制与平台策略

| 机制 | 平台 | 出现位置 | 实现载体 |
|---|---|---|---|
| 传统动词（HKCU 静态动词） | Win10；Win11 仅作回退 | 经典菜单第一层（Win11 上在「显示更多选项」里） | 注册表 `HKCU\Software\Classes\*\shell\HiEditorPro` |
| 稀疏 MSIX 包 + IExplorerCommand | Win11（build 22000+） | **新版菜单第一层 + 经典菜单**（同一条目，两处显示） | `sparse\HiEditorProContextMenu.msix` 注册 + `hieditor_menu.dll` COM 组件 |

关键策略（`crates/hieditor-gui/src/shell_menu.rs` 的 `register_context_menu`）：

- **Win11 只注册稀疏包**。包条目在两个菜单里各显示一次，传统动词再注册就会在
  经典菜单里造成重复条目。稀疏包注册成功后，程序会主动清理残留的传统动词。
- **Win10 只注册传统动词**。Win10 没有新版菜单，且稀疏包清单 `MinVersion=10.0.22000.0`
  强行注册只会报 `0x80073CFD`。
- 稀疏包注册失败且包当前未注册时，才回退注册传统动词（保证至少经典菜单可用）；
  若包仍注册着（例如证书未确认但旧包可用），不补动词以免重复。

## 1. 文件清单

| 文件 | 性质 | 说明 |
|---|---|---|
| `sparse\AppxManifest.xml` | 版本管理 | 包清单：Identity（`HiEditorProContextMenu` / `CN=HiEditorPro`）、`uap10:AllowExternalContent=true`、`com:ComServer`（CLSID `{2E7EED97-9BAE-4334-8DA1-8DE129DA7024}`）、`desktop4:FileExplorerContextMenus` |
| `sparse\assets\*.png` | 版本管理 | 清单引用的菜单/磁贴图标 |
| `sparse\HiEditorProContextMenu.msix` | 构建产物（gitignore） | 已签名的稀疏包（只含清单+图标，31 KB 左右） |
| `sparse\HiEditorPro.cer` | 构建产物（gitignore） | 签名证书公钥，随发行包分发用于信任导入 |
| `signing\hieditor-signing.pfx` | 本地私有（gitignore） | **固定签名证书**（唯一来源），勿入库勿外传；CI 经 GitHub Secret `SIGNING_PFX` 注入同一文件 |
| `signing\hieditor-signing.pfx.b64` | 本地私有（gitignore） | pfx 的 Base64，用于填写 GitHub Secret |
| `signing\hieditor-signing.cer` | 版本管理 | 当前证书的公钥存档（发行包里的 `.cer` 由 build-msix.ps1 从 pfx 派生，不依赖此副本） |
| `signing\create-signing-cert.ps1` | 版本管理 | 首次初始化固定证书（CN=HiEditorPro，10 年有效期；已有 pfx 时跳过） |
| `signing\reset-signing-cert.ps1` | 版本管理 | 生成新证书覆盖 pfx/.cer（旧证书作废，需同步提交 cer 并更新 Secret） |
| `signing\build-msix.ps1` | 版本管理 | MakeAppx 打包 + SignTool 签名 |
| `target\release\HiEditorPro.exe` | 编译产物 | 主程序；清单 `Executable` 指向它，菜单用它启动并显示图标 |
| `target\release\hieditor_menu.dll` | 编译产物 | IExplorerCommand 进程内 COM 组件（com:SurrogateServer，跑在 dllhost 代理进程里） |

注册/运行后在 `sparse\`（或发行包解压目录）产生的运行数据：
`microsoft.system.package.metadata\`（系统生成的注册状态）、`recent_files.json`、
`hieditor.conf`（写在 exe 旁边），均已 gitignore。

## 2. 编译

### 2.1 程序与 COM 组件

```bat
rebuild.bat            :: 即 cargo build --release --workspace
```

产物 `HiEditorPro.exe`（GUI）与 `hieditor_menu.dll`。图标显示链路：新版菜单调用
COM 组件的 `IExplorerCommand::GetIcon`（返回「exe 完整路径,0」），经典菜单动词读
注册表 `Icon` 值（同样是 exe,0）——两者都要求 **exe 旁边能找到主程序** 且 exe 带
内嵌图标资源（由 `build.rs` 的 winresource 嵌入）。

### 2.2 签名稀疏包（免开发者模式的关键）

```powershell
# 一次性：初始化固定证书（已有 pfx 时跳过；更换证书用 reset-signing-cert.ps1 [-Force]）
powershell -NoProfile -ExecutionPolicy Bypass -File signing\create-signing-cert.ps1

# 每次出包：打包 + 签名（release.bat / package.bat 会自动调用）
powershell -NoProfile -ExecutionPolicy Bypass -File signing\build-msix.ps1 `
    -ManifestDir sparse -OutMsix sparse\HiEditorProContextMenu.msix -CertOut sparse\HiEditorPro.cer
```

证书为**固定证书**：`signing\hieditor-signing.pfx` 是唯一来源，本地与 CI（GitHub
Secret `SIGNING_PFX` = pfx 的 Base64）使用同一张，所有版本签名一致，用户只需
首次注册时信任一次。CI 侧由 workflow 从 Secret 还原 pfx，不依赖机器证书库。

`build-msix.ps1` 的动作：从干净暂存目录（仅清单+图标，exe/dll 不进包）用
`makeappx pack /o /nv /d` 打包（`/nv` 跳过清单引用文件检查），`signtool sign /fd SHA256`
用 `signing\hieditor-signing.pfx` 签名。依赖 Windows SDK
（`C:\Program Files (x86)\Windows Kits\10\bin\<版本>\x64`）。

`release.bat` / `package.bat` 出包时自动执行上述步骤并把
`sparse\{*.msix, *.cer}` 放进 zip；构建失败仅警告，发行包回退为未签名清单
（注册退化为开发者模式路径）。

## 3. 注册

入口二选一：`HiEditorPro --register`，或 GUI 内 **设置 → 注册右键菜单打开**
（内部同走 `register_context_menu()`）。免管理员、可重复执行、随目录迁移。

### 3.1 Win11 稀疏包注册（主路径）

程序按序执行（签名包存在时走 `register_signed_package`）：

1. **写辅助 CLSID**：`HKCU\Software\Classes\CLSID\{2E7EED97-…}\InprocServer32`
   指向 exe 旁的 `hieditor_menu.dll`（菜单激活实际走清单 `com:ComServer`，此项
   供 CoCreateInstance 直测，二者缺一不可）。
2. **直接注册**（不预删除，尽量不破坏已注册的可用状态）：

   ```powershell
   Add-AppxPackage -Path sparse\HiEditorProContextMenu.msix -ExternalLocation <exe 所在目录>
   ```

   - 首次注册：签名证书未受信 → 报 `0x800B0109/0x800B010A` → **弹一次 UAC**，
     管理员执行 `certutil -addstore TrustedPeople sparse\HiEditorPro.cer` 把证书
     导入**本机**受信任存储后自动重试。AppXSvc 以 SYSTEM 运行，签名校验**只查
     LocalMachine 存储——导入 CurrentUser\TrustedPeople 无效**（微软文档与此不符，
     实测结论）。UAC 被取消则原样返回，已注册的旧包不受影响。
   - 同版本已注册（`0x80073CFB/0x80073CF9`）：先
     `Get-AppxPackage HiEditorProContextMenu | Remove-AppxPackage` 再重试注册（幂等升级）。
3. **清理传统动词**：注册成功后删除 `HKCU\Software\Classes\*\shell\HiEditorPro`，
   避免经典菜单重复。
4. 注册成功后系统在包目录生成 `microsoft.system.package.metadata\`。

### 3.2 传统动词注册（Win10 主路径 / Win11 回退）

原生注册表 API（不经 reg.exe 子进程）写当前用户作用域，免管理员：

| 注册表值 | 内容 |
|---|---|
| `HKCU\Software\Classes\*\shell\HiEditorPro`（默认值） | 菜单名「用 HiEditor Pro 打开」 |
| `…\shell\HiEditorPro\Icon` | `<exe 路径>,0` |
| `…\shell\HiEditorPro\command` | `"<exe 路径>" "%1"` |

多选文件时每个文件一个标签页（exe 自行解析多个 `"%1"` 参数）。

### 3.3 未签名开发布局回退

exe 旁 `sparse\` 没有 `.msix`（例如只跑过 `cargo build`）时，走
`register_unsigned_package`：把 exe/dll 复制进 `sparse\`、对齐清单 `Executable`，
然后 `Add-AppxPackage -Register sparse\AppxManifest.xml -ExternalLocation sparse\`。
清单含 `AllowExternalContent=true`，**`-Register` 必须带 `-ExternalLocation`**
（否则报 `0x80073CF9`「必须使用外部位置安装」）。此路径未签名，**需系统开启
开发者模式**（否则 `0x80073CFF`）；Win10 上跳过稀疏包注册。

## 4. 删除注册

`HiEditorPro --unregister`（幂等，未注册项不报错）：

1. 删传统动词键：`RegDeleteTreeW(HKCU\Software\Classes\*\shell\HiEditorPro)`。
2. 删辅助 CLSID 键：`RegDeleteTreeW(HKCU\Software\Classes\CLSID\{2E7EED97-…})`。
3. 移除稀疏包：`Get-AppxPackage HiEditorProContextMenu | Remove-AppxPackage`
   （Win11），同时清掉 `microsoft.system.package.metadata\`。

**签名证书不会自动移除**：`LocalMachine\TrustedPeople` 里的 `HiEditorPro` 证书
需要时手动清理（管理员）：

```bat
certutil -delstore TrustedPeople HiEditorPro
```

## 5. 排查

| 检查点 | 位置/命令 |
|---|---|
| 包注册状态 | `Get-AppxPackage HiEditorProContextMenu`（看 `SignatureKind`、`IsDevelopmentMode`、`InstallLocation`） |
| 部署失败详情 | 事件查看器：应用程序和服务日志 → Microsoft → Windows → **AppxDeployment-Server**；或 `Get-AppPackageLog -ActivityID <id>` |
| COM 组件日志 | `%TEMP%\hieditor_menu.log`（加载与 Invoke） |
| 主程序参数日志 | `%TEMP%\hieditor_pro.log`（菜单调用链验证点） |

| 错误码 | 含义 | 处置 |
|---|---|---|
| `0x800B0109` / `0x800B010A` | 签名证书未受信 | 首次注册弹 UAC 自动导入；取消后重跑注册即可 |
| `0x80073CFB` / `0x80073CF9` | 同版本已注册 / 需外部位置 | 程序自动先移除再注册；手动可先 `Remove-AppxPackage` |
| `0x80073CFF` | 未签名注册但未开开发者模式 | 开发者模式，或改用发行包内的签名包注册 |
| `0x80073CFD` | 系统版本低于清单 `MinVersion` | Win10 上跳过稀疏包注册属预期行为 |
| 菜单无图标 | `GetIcon` 未返回有效路径 | 检查 exe 是否在 DLL 同目录、exe 是否带内嵌图标；历史 bug：宽字符串缺 0 终止符导致恒返 `E_FAIL`（已修复） |
