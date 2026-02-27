# neosh v0.1.0 交付测试文档

## 1. 目标

验证 `neosh` / `neoshd` 在 v0.1.0 的关键交付能力：

- 新建会话并交互
- `DETACH` 后可 `RESUME`
- `resume` 前必须完成 `renew-auth` + `AUTH`
- `auth_token` 单次消费语义
- 指纹校验在 `AUTH` 前生效

---

## 2. 测试前准备

### 2.1 环境要求

- 一台客户端机器（运行 `neosh`）
- 一台服务端机器（可 SSH 登录，运行 `neoshd`）
- 双方网络可达（UDP/QUIC 端口可通）

### 2.2 版本与二进制检查

在客户端：

```bash
neosh version
```

在服务端：

```bash
neoshd version
```

期望：

- `neosh/0.1.0`
- `neoshd/0.1.0`

### 2.3 测试变量（执行前填写）

```bash
export NEO_HOST="<server-host-or-ip>"
export NEO_USER="<ssh-user>"
export NEO_TARGET="${NEO_USER}@${NEO_HOST}"
```

---

## 3. 交付测试用例

### TC-01 新建会话与交互（Happy Path）

命令：

```bash
neosh connect "${NEO_TARGET}"
```

操作：

- 进入远端交互后执行：`echo "neosh-e2e"` 并回车。

期望：

- 成功进入交互会话。
- 终端输出包含 `neosh-e2e`。
- 服务端日志可见 `HELLO -> AUTH -> ATTACH`（或等价事件顺序）。

失败判定：

- 无法进入会话、会话立即退出、或日志时序不符合协议。

---

### TC-02 Detach 后 Resume（主流程）

前置：

- 已完成 TC-01，并拿到 `session_id`（来自客户端缓存或启动输出）。

步骤：

1. 在交互会话触发 detach（例如 `Ctrl-] d`）。
2. 在客户端执行：

```bash
neosh resume --session-id <session_id>
```

期望：

- 成功恢复到原会话。
- 服务端日志显示新连接流程为 `HELLO -> AUTH -> RESUME`。
- 恢复后仍可继续输入命令并收到输出。

失败判定：

- 直接新建了会话（非原会话恢复）或恢复路径缺少 `AUTH`。

---

### TC-03 手动验证 renew-auth 返回

命令：

```bash
ssh "${NEO_TARGET}" 'neoshd renew-auth --session-id <session_id> --user "$USER"'
```

期望：

- 返回 JSON，至少包含：
  - `session_id`
  - `auth_token`
  - `auth_token_expires_in_seconds`
  - `quic_addr`
  - `cert_fingerprint`

失败判定：

- 缺字段、非 JSON、或返回了新的 `session_id`。

---

### TC-04 auth_token 单次消费

目标：

- 验证同一个 `auth_token` 不能重复用于 `AUTH`。

建议执行方式：

1. 先执行 TC-03 取得一枚 `auth_token`。
2. 使用该 token 完成一次成功连接（可通过 `neosh resume` 路径触发）。
3. 立即重复使用同一 token（通过测试工具/调试脚本或重复注入）再发起一次 `AUTH`。

期望：

- 第一次成功，第二次返回 `AUTH_FAILED`。
- 服务端日志包含 token 被拒绝/重复消费记录（如 `token_rejected`）。

失败判定：

- 同一 token 可重复成功通过 `AUTH`。

---

### TC-05 auth_token 过期处理

步骤：

1. 获取短时有效 `auth_token`（TC-03）。
2. 等待超过 `auth_token_expires_in_seconds`。
3. 用过期 token 发起连接/认证。

期望：

- 返回 `AUTH_FAILED`。
- 客户端恢复策略正确：
  - `connect` 场景：重新执行 `neoshd new` 获取 token。
  - `resume` 场景：重新执行 `neoshd renew-auth --session-id ...` 获取 token。

失败判定：

- 过期 token 仍被接受，或恢复策略走错路径。

---

### TC-06 指纹校验失败保护

目标：

- 验证客户端在 `AUTH` 前完成指纹校验，指纹不一致时立即失败。

步骤（任选其一）：

- 修改客户端缓存中的 `cert_fingerprint` 为错误值再执行 `resume`；或
- 让 bootstrap 返回值与实际服务端证书不一致（测试环境模拟）。

期望：

- 客户端在 `AUTH` 前中止。
- 输出明确的 fingerprint mismatch / trust error。
- 服务端不应出现该次连接的成功 `AUTH_OK`。

失败判定：

- 指纹不一致仍进入 `AUTH` 或成功附着会话。

---

## 4. 验收标准（交付门槛）

以下条目必须全部通过：

- TC-01 通过
- TC-02 通过
- TC-03 通过
- TC-04 通过
- TC-05 通过
- TC-06 通过

结论判定：

- 全通过：v0.1.0 交付测试通过。
- 任一失败：v0.1.0 交付测试不通过，需修复后回归。

---

## 5. 测试记录模板

```text
Build/Commit:
Tester:
Date:
Env:

TC-01: PASS/FAIL  Notes:
TC-02: PASS/FAIL  Notes:
TC-03: PASS/FAIL  Notes:
TC-04: PASS/FAIL  Notes:
TC-05: PASS/FAIL  Notes:
TC-06: PASS/FAIL  Notes:

Overall: PASS/FAIL
```

