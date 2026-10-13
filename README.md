# NEU 校园网 · Rust Web 管理服务

当前分支为 **`rust_arm32`**，面向 ARMv7（32 位硬浮点、glibc）Linux，完整保留 `rust_x86` 的认证、网页、存储和部署功能。默认端口为 **11099**；x86_64 主机使用 `rust_x86` 分支，64 位 ARM 用户空间使用 `rust_arm64` 分支。

使用 Rust 重写东北大学校园网认证逻辑，提供账号保存、状态检测、一键登录/注销、操作记录、背景和外观设置。网页、SVG 图标和默认背景内置于原生二进制，运行时无需 Python、Node.js、Docker 或 CDN。

## 功能与默认行为

- 首页区分在线、离线、检测失败，显示当前在线账号、网络出口、在线时长和最近检测时间。
- 登录/注销后复查状态，不把 HTTP 200 当成成功；重复操作协调执行。
- 保存账号密码；修改时留空密码保留原密码。首页显隐按钮临时显示已保存密码，30 秒后隐藏。
- 后台默认每 15 秒检测，通过 SSE 同步多个网页。关闭网页仍可检测，不会自动登录或重连。
- 首页最近三条记录，完整记录分页，每页 20 条，最多保留 1000 条。
- PNG/JPEG/WebP 背景上传（最大 10 MB），选择服务器/NAS 挂载目录图片，恢复默认背景。
- 浅色/深色/跟随系统主题、透明度、背景模糊和手机布局；设置重启后保留。
- 自定义 HTTPS 认证地址、ac_id、RSA 公钥、请求超时与检测间隔。
- SQLite 持久保存，AES-256-GCM 加密密码；数据库和密钥权限 0600，数据目录 0700。

服务认证的是**运行服务的设备所使用的校园网出口**，不是访问网页的各个客户端。当前出口已在线时直接返回已在线；首页在线账号来自网关，可能与保存但尚未登录的账号不同。

按当前需求，网页**不设置管理密码**。能访问服务端口的设备可以操作认证和设置，并通过显隐按钮查看保存的密码。默认不向公网发布。

## 构建与本机运行

需要支持 NEON 的 ARMv7 Linux（32 位硬浮点用户空间）、Rust 1.96、C/C++ 编译器、CMake、pkg-config。SQLite 随程序编译，TLS 使用 Rustls，无需 OpenSSL 动态库。本分支已用 QEMU 验证；当前工具链在仅有 VFPv3-D16、禁用 NEON 的 CPU 模拟环境中出现非法指令，因此该环境不在本发布版本的支持范围内。

```bash
sudo apt-get install build-essential cmake pkg-config ca-certificates
rustup target add armv7-unknown-linux-gnueabihf
cargo build --release --locked --target armv7-unknown-linux-gnueabihf
./target/armv7-unknown-linux-gnueabihf/release/neu-login
```

访问 <http://127.0.0.1:11099/>；局域网访问 `http://本机IP:11099/`。首次进入“账号设置”填写统一身份认证账号密码，保存后回首页登录。启动时不自动登录或注销。

```bash
./target/armv7-unknown-linux-gnueabihf/release/neu-login --listen 0.0.0.0:11099 --data-dir ./data
./target/armv7-unknown-linux-gnueabihf/release/neu-login --image-dir /srv/neu-backgrounds
./target/armv7-unknown-linux-gnueabihf/release/neu-login --help
```

| 参数 | 环境变量 | 默认值 |
| --- | --- | --- |
| `--listen` | `NEU_LISTEN` | `0.0.0.0:11099` |
| `--data-dir` | `NEU_DATA_DIR` | `data` |
| `--image-dir` | `NEU_IMAGE_DIR` | 未配置 |

页面时间为 Asia/Shanghai。在线时长优先使用网关上线时间，缺少时标记“已观察在线时长”；检测失败显示未知状态。

### 在 x86_64 Linux 交叉编译

下面以 Debian/Ubuntu 为例；其他发行版安装对应 ARMv7 硬浮点 GNU 工具链。交叉编译期间不运行生成的 ARMv7 程序，也不替换当前 x86_64 服务。

```bash
sudo apt-get install gcc-arm-linux-gnueabihf g++-arm-linux-gnueabihf cmake pkg-config
rustup target add armv7-unknown-linux-gnueabihf
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
export CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
export CXX_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-g++
export AR_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-ar
./scripts/package.sh
```

打包脚本从 Cargo.toml 读取版本，支持 `CARGO_TARGET_DIR`，不需要执行 ARMv7 二进制。安装脚本支持 `armv7l` 和使用 32 位用户空间的 `armv8l`，安装前检查 NEON/ASIMD 标志，拒绝不兼容的 CPU。此目标使用 glibc，不适用于 ARMv6、软浮点 ABI 或基于 musl 的 Alpine/OpenWrt。[Rust ARMv7 GNU 目标说明](https://doc.rust-lang.org/rustc/platform-support/armv7-unknown-linux-gnueabi.html)

## Docker 镜像与同架构主机部署

Dockerfile 使用多阶段构建：在官方 Rust 1.96.0 镜像中编译，运行镜像仅保留二进制、CA 证书、健康检查和时区所需组件。无需在目标主机安装 Rust，也不依赖目标主机的 glibc 版本；构建和运行阶段均使用 Debian Bookworm。

Dockerfile、Compose 和导出脚本默认构建 `linux/arm/v7`，镜像标签为 `neu-login:2.0.0-arm32`。目标主机需要 ARMv7 Linux Docker。ARMv7 主机原生构建；在 x86_64 主机构建需要支持 ARMv7 的 Buildx/QEMU 构建器，模拟构建会较慢。[Docker 多平台构建说明](https://docs.docker.com/build/building/multi-platform/)

首次构建需联网下载基础镜像、系统包和 Cargo 依赖；目标主机加载导出的镜像后可离线启动。

### 本机构建和启动

构建需要 Docker Buildx 插件，ARM32 分支不使用经典构建器回退。先运行 `docker buildx version`；使用 sudo 构建时必须验证 `sudo docker buildx version`，普通用户目录中的插件不一定对 root 可见。Dockerfile 在安装系统包前检查基础镜像的 `armhf` 架构。

```bash
docker compose build
docker compose up -d
docker compose ps
docker compose logs -f --tail 100
```

访问 `http://主机IP:11099/`。Compose 默认使用 `host` 网络，与宿主机共享网络出口，不配置端口映射；镜像的 `EXPOSE` 是端口声明，不会自动发布端口。[Docker host 网络说明](https://docs.docker.com/engine/network/drivers/host/)

如果在目标 ARMv7 设备从原生服务切换到 Docker，先停止原生服务，释放 11099 端口：

```bash
systemctl --user disable --now neu-login
# 若安装的是系统级服务，则改用 sudo systemctl disable --now neu-login
docker compose up -d
```

该步骤不会自动迁移原生服务数据；Docker 默认使用独立数据卷，首次在网页填写账号。构建镜像本身不会停止当前原生服务。

### 一键生成可迁移镜像包

```bash
./scripts/docker-export.sh
# Docker 需要管理员权限时：sudo ./scripts/docker-export.sh
```

脚本构建 `neu-login:2.0.0-arm32` 并导出到 `release/`，同时检查镜像平台，拒绝导出非 ARMv7 镜像：

```text
release/neu-login-2.0.0-docker-linux-arm-v7.tar.gz
release/neu-login-2.0.0-docker-linux-arm-v7.tar.gz.sha256
release/compose.yaml
```

复制这三个文件到同架构目标主机的同一目录，执行：

```bash
sha256sum -c neu-login-2.0.0-docker-linux-arm-v7.tar.gz.sha256
docker load -i neu-login-2.0.0-docker-linux-arm-v7.tar.gz
docker compose up -d --no-build --pull never
```

加载后无需拉取基础镜像、构建工具或源码。镜像包包含运行镜像层和标签，不包含账号与运行数据。[镜像导出](https://docs.docker.com/reference/cli/docker/image/save/)、[压缩镜像加载](https://docs.docker.com/reference/cli/docker/image/load/)

目标主机若未安装 Compose，也可直接运行已加载的镜像：

```bash
docker run -d --name neu-login --restart unless-stopped --network host \
  --mount type=volume,source=neu-login_neu_data,target=/data \
  --platform linux/arm/v7 neu-login:2.0.0-arm32
```

导出脚本使用 `docker buildx build --platform linux/arm/v7 --load`；缺少 Buildx 时立即报错，不进入经典构建器。通过 sudo 执行时，生成的文件归调用用户所有，便于复制。

在 x86_64 主机首次配置 Buildx（以下使用 sudo，后续构建也使用 sudo）：

```bash
# 已配置 Docker 官方 apt 源时，安装其插件包：
sudo apt-get update
sudo apt-get install docker-buildx-plugin
sudo docker buildx version

# 首次创建；已有同名构建器时直接 use：
sudo docker buildx create --name neu-arm32-builder --driver docker-container --use --bootstrap
# 后续使用：sudo docker buildx use neu-arm32-builder
sudo docker buildx inspect --bootstrap
```

确认 `Platforms` 包含 `linux/arm/v7` 后运行 `sudo ./scripts/docker-export.sh`。若没有 ARMv7 平台或出现 `exec format error`，注册模拟器后重新检查：

```bash
sudo docker run --privileged --rm tonistiigi/binfmt --install arm
sudo docker buildx inspect --bootstrap
```

如果系统没有配置 Docker 官方 apt 源，按 [Buildx 安装说明](https://github.com/docker/buildx#installing) 安装插件；使用 sudo 时可安装到系统插件目录 `/usr/local/lib/docker/cli-plugins/docker-buildx`，权限设为 0755。仅安装插件即可，不需要重装 Docker Engine 或删除现有镜像、数据卷。

若日志在 ARM32 构建期间下载 `amd64` 软件包，随后提示 `does not provide the specified platform (linux/arm/v7)`，说明基础镜像平台不匹配，应切换至上述 Buildx 构建器；重新打标签不能把 x86 镜像转换成 ARM32。

使用 Buildx 手动构建时：

```bash
docker buildx inspect --bootstrap
docker buildx build --platform linux/arm/v7 --load -t neu-login:2.0.0-arm32 .
docker image inspect --format '{{.Os}}/{{.Architecture}}/{{.Variant}}' neu-login:2.0.0-arm32
# 应输出 linux/arm/v7
```

可指定标签与输出目录：

```bash
./scripts/docker-export.sh neu-login:2.0.0-arm32 /tmp/neu-login-arm32-images
# 自定义标签在目标主机也需指定：
NEU_IMAGE=neu-login:2.0.0-arm32 docker compose up -d --no-build --pull never
```

### 数据、图片目录与运行参数

- 容器以非 root 用户 `10001:10001` 运行，账号、密钥、设置、背景和记录保存在命名卷 `neu-login_neu_data` 的 `/data` 中。
- `docker compose down` 保留数据卷；`docker compose down -v` 会删除账号及其他数据，日常停机不要使用 `-v`。
- 升级时重新构建或加载新镜像，再执行 `docker compose up -d`；保留原数据卷。
- 若用宿主机目录替代命名卷，先为目录设置 UID/GID `10001:10001` 所有权和 0700 权限，再挂载到 `/data`；程序需要创建、写入并设置文件权限。
- 需要 NAS/服务器图片时，在 Compose 的 `environment` 添加 `NEU_IMAGE_DIR: /backgrounds`，并启用图片目录的只读挂载；确保容器用户可读取并遍历该目录。

```yaml
# 添加到 compose.yaml 对应的 environment 与 volumes：
environment:
  NEU_IMAGE_DIR: /backgrounds
volumes:
  - /srv/neu-backgrounds:/backgrounds:ro
```

修改监听端口可使用 `NEU_LISTEN=0.0.0.0:11099 docker compose up -d`，同时更新访问地址；host 模式不使用 `ports`。健康检查跟随监听端口，不要求校园网已经认证。

镜像与容器配置不包含管理密码。构建上下文采用白名单，仅发送 Rust 源码、Cargo 文件和网页资源，排除 Git 历史、数据库、密钥和其他本机文件；导出的镜像包仍由 `.gitignore` 的 `release/` 规则排除，不作为源码提交。

## systemd 系统服务

完成构建后安装，或解压发布包后执行同一脚本：

```bash
sudo ./scripts/install.sh
systemctl status neu-login
sudo systemctl restart neu-login
journalctl -u neu-login -n 100 --no-pager
curl --noproxy '*' http://127.0.0.1:11099/healthz
```

程序在 `/usr/local/bin/neu-login`，服务在 `/etc/systemd/system/neu-login.service`，环境配置在 `/etc/neu-login.env`，数据在 `/var/lib/neu-login/`。使用专用系统用户 `neu-login`，开机启动、异常重启。健康检查表示 Web 服务工作正常，不要求校园网在线。

更改端口或服务器图片目录时编辑 `/etc/neu-login.env` 后重启：

```ini
NEU_LISTEN=0.0.0.0:11099
NEU_DATA_DIR=/var/lib/neu-login
NEU_IMAGE_DIR=/srv/neu-backgrounds
```

图片目录须存在，服务用户须能读取并遍历。NAS 共享先挂载到本机目录。仅列出指定目录下的普通 PNG/JPEG/WebP 文件，不递归、不接受符号链接，网页不能指定任意文件路径。明确配置的目录不存在时拒绝启动，避免忽略配置错误。

## 没有 sudo 时使用用户服务

```bash
./scripts/install.sh --user
systemctl --user status neu-login
systemctl --user restart neu-login
journalctl --user -u neu-login -n 100 --no-pager
```

用户程序为 `~/.local/bin/neu-login`，配置为 `~/.config/neu-login/environment`，数据为 `~/.local/share/neu-login/`，unit 为 `~/.config/systemd/user/neu-login.service`。支持标准 XDG 目录变量。

用户服务默认登录后启动。要在未登录时启动，需管理员启用 linger：

```bash
sudo loginctl enable-linger "$USER"
```

切换至系统服务时先停止用户服务，避免端口冲突。迁移已有账号需要同时复制数据库、密钥和背景：

```bash
systemctl --user disable --now neu-login
sudo ./scripts/install.sh
sudo systemctl stop neu-login
sudo cp -a "$HOME/.local/share/neu-login/." /var/lib/neu-login/
sudo chown -R neu-login:neu-login /var/lib/neu-login
sudo systemctl start neu-login
```

## 升级、备份、迁移、卸载

再次运行同一种安装命令升级，数据保留。安装脚本备份上一份程序，新版本健康检查失败时恢复旧程序。

备份时停止服务，再整体复制数据目录，包含 `neu.db`、`secret.key`、`backgrounds/`。数据库和密钥必须成对恢复；已有数据库但密钥丢失时，服务拒绝静默生成替代密钥。

```bash
# 默认保留数据和环境配置
sudo ./scripts/uninstall.sh
./scripts/uninstall.sh --user

# 明确指定后才删除账号、密钥、背景、设置与记录
sudo ./scripts/uninstall.sh --purge-data
./scripts/uninstall.sh --user --purge-data

# 打包
./scripts/package.sh
```

生成 `release/neu-login-2.0.0-arm32-linux.tar.gz` 及 SHA256 文件，包含二进制、systemd 配置、安装/卸载脚本和说明，不包含账号、数据库、密钥或测试数据。构建目录、发布包和运行数据已在 `.gitignore` 中排除。

原生发布包要求目标设备 glibc 版本不低于编译工具链使用的版本；交叉编译用的 Ubuntu 24.04 工具链产生的程序不可直接承诺兼容旧发行版。可在目标设备从源码构建，或使用基于 Debian Bookworm 的 ARMv7 Docker 镜像。迁移时整体复制数据目录，SQLite 数据和密钥格式与 x86_64 版本相同。

## 认证流程

1. 查询 `rad_user_info`，区分在线、明确离线、无法检测。
2. 访问门户读取实际网络参数并绑定 CAS service。
3. 解析 `lt/execution`，用 RSA PKCS#1 v1.5 加密 UTF-8 的 `username + password`。
4. 在同一 Cookie 会话提交 CAS 表单，捕获 SSO ticket 跳转。
5. 直接将 ticket 交给 `/v1/srun_portal_sso`，不先用 HTML 门户消耗 ticket。
6. 复查状态，必要时短暂等待后再查，以实际状态确认结果。

每次认证独立会话、禁用系统代理，跳转限制在配置的认证服务器且最多 10 次。认证、刷新、配置写入统一协调，冲突返回 409；浏览器关闭后已提交的认证任务仍会完成。

自定义地址要求 HTTPS，保持 TLS 校验。验证码、账号锁定或页面结构变化会明确提示。日志和错误响应不包含密码、Cookie、ticket 或原始认证响应。

## API

写操作需 `X-NEU-Request: 1`；JSON 写入使用 `Content-Type: application/json`。存在 Origin 时须与 Host 同源。不开放跨域。响应禁用缓存。

| 接口 | 功能 |
| --- | --- |
| `GET /api/status`、`POST /api/status/refresh` | 最近状态 / 立即检测 |
| `POST /api/login`、`POST /api/logout` | 认证、注销并复查 |
| `GET/PUT /api/account` | 账号摘要 / 保存账号 |
| `POST /api/account/reveal` | 临时读取已保存密码 |
| `GET/PUT /api/settings` | 读取 / 保存完整设置 |
| `GET /api/logs?page=1` | 分页记录 |
| `GET /api/events` | SSE `update`，含状态、账号摘要、设置和最近记录 |
| `GET/POST /api/backgrounds` | 图片列表 / multipart `file` 上传 |
| `GET /api/backgrounds/{id}` | 读取列表内图片 |
| `GET /healthz` | 服务健康和版本 |

账号请求：`{"username":"学号","password":"密码"}`。修改时密码为 `null` 或空字符串表示保留；首次不能省略。状态 `state` 为 `online/offline/unknown`，`unknown` 不能当成离线。

## 验证与排障

```bash
cargo test --locked --target armv7-unknown-linux-gnueabihf
cargo clippy --all-targets --locked --target armv7-unknown-linux-gnueabihf -- -D warnings
cargo fmt --all -- --check
node --check web/app.js
```

Rust 测试使用本地模拟 CAS/IPGW，验证 RSA 解密、UTF-8 字节长度、Cookie、门户参数、ticket、错误密码、未知响应、超时、注销未生效、重复操作、加密持久化和同源校验；不读取真实账号、不访问真实认证服务器。

在 x86_64 主机运行 ARMv7 测试，额外安装 `qemu-user-static`，保持上面的交叉编译环境变量，并设置 runner：

```bash
sudo apt-get install qemu-user-static
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUNNER='qemu-arm-static -L /usr/arm-linux-gnueabihf'
cargo test --locked --target armv7-unknown-linux-gnueabihf
```

QEMU 用于验证 ARMv7 指令与测试流程，实际校园网认证仍需在目标 ARMv7 设备填写账号后验证。

- 网页不可达：检查服务、端口占用、防火墙和局域网隔离。
- 脚本报 `invalid option name`、`pipefail` 或解释器不存在：检查是否被转成 Windows CRLF 换行。在项目目录运行 `sed -i 's/\r$//' scripts/*.sh` 和 `chmod +x scripts/*.sh`，再执行 `sudo ./scripts/docker-export.sh`。脚本需要 Bash，不能用 `sh` 执行。仓库的 `.gitattributes` 已固定 Shell 脚本为 LF 换行；复制文件或解压时也应保留此格式。
- 检测失败：检查运行设备的校园网、DNS、TLS 和认证服务器可达性。
- 已在线但没保存账号：网关按当前出口返回状态；保存账号后才能在掉线时手动登录。
- 登录页面变化：检查学校的页面、公钥、SSO 参数；高级设置可覆盖认证参数。
- 图片无法读取：检查 NAS 挂载及服务用户权限，修改环境配置后重启。
- 密钥缺失：恢复数据库和密钥的成对备份，保留原数据。

安装脚本不更改其他服务、不自动开放防火墙。
