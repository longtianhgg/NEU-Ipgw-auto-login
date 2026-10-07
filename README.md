## 自动认证东北大学校园网脚本

本项目用于在命令行、服务器、软路由等环境中自动完成东北大学校园网认证。

当前版本已参考 [NEU_IPGW](https://github.com/longtianhgg/NEU_IPGW) 的新认证流程进行更新，重点适配统一身份认证从旧的明文 `rsa=username+password+lt` 方式切换到 **RSA PKCS#1 v1.5 加密** 后的登录流程。

### 主要变化

- 使用当前统一身份认证 RSA 公钥生成 `rsa` 参数。
- 不再依赖旧版固定 `ac_id=15/1` 轮询逻辑，优先读取当前 IPGW 门户实际跳转参数。
- 先完成统一身份认证，再获取 IPGW SSO ticket，最后访问 `/v1/srun_portal_sso` 完成校园网登录。
- 登录前先查询 `rad_user_info`，已经在线时直接返回，不重复认证。
- HTML 参数解析不再使用脆弱的字符串 `index()` 截取。
- 保留原来的 `Ipgw_login` 类名别名，已有自动化脚本无需改导入名称。
- 仅依赖 Python 标准库和 `requests`，不额外要求安装加密库。

### 1. 填写账号密码

在 `config.ini` 中填写统一身份认证账号密码：

```ini
[info]
StudentID = 20001234
password = 12345678
```

建议不要把真实账号密码提交到 Git 仓库。

### 2. 环境要求

- Python 3
- requests

安装依赖：

```bash
python3 -m pip install requests
```

### 3. 运行

```bash
python3 login.py
```

成功时退出码为 `0`，认证失败时退出码为 `1`，配置错误时退出码为 `2`，因此可以直接配合 cron、systemd、OpenWrt 定时任务或其他自动化脚本使用。

### 认证流程

当前脚本的核心流程为：

```text
检查 rad_user_info
        │
        ├─ 已在线 → 直接结束
        │
        └─ 未在线
             │
             ├─ GET pass.neu.edu.cn/tpass/login
             │      └─ 获取 lt / execution
             │
             ├─ RSA(username + password)
             │      └─ POST 统一身份认证
             │
             ├─ 访问 IPGW portal 获取当前网络参数
             │
             ├─ 通过统一身份认证获取 SSO ticket
             │
             └─ GET https://ipgw.neu.edu.cn/v1/srun_portal_sso?...&ticket=...
                    └─ 完成校园网认证
```

### 说明

旧版脚本最后一次更新于 2023-11-11，当时主要处理 `checkacid` / `mysession` 变化。之后统一身份认证又更新了账号提交方式，旧版：

```python
rsa = username + password + lt
```

已无法适应当前认证方式。

本次更新参考 NEU_IPGW 当前实现中的 RSA 公钥及登录链路。若学校后续再次更换统一身份认证公钥或 SSO 接口，脚本仍可能需要继续调整。
