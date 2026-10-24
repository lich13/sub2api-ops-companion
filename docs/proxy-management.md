# IP 管理

功能页通过当前连接的 Sub2API 管理接口操作同一代理目录，以 0.2.11 的字段与到期规则为准。认证、到期处理及备用代理均由上游持久化，不新增 Companion 状态文件或后台任务。

接口前缀为 `/api/desktop/v1`。原 `GET /proxies` 保持账号代理选择器的精简响应；诊断仍使用 `POST /proxies/:id/test` 与 `POST /proxies/:id/quality-check`。

| 接口 | 行为 |
| --- | --- |
| `GET /proxy-management` | 分页、搜索、协议与状态筛选、排序；返回认证存在标志 |
| `POST /proxy-management` | 新增代理 |
| `GET /proxy-management/:id` | 不含认证明文的代理详情 |
| `PUT /proxy-management/:id` | 部分更新；省略密码保留，空字符串清除 |
| `DELETE /proxy-management/:id` | 删除；关联账号与备用关系限制由上游校验 |
| `GET /proxy-management/:id/credentials` | 显式查看或复制时读取认证 |
| `GET /proxy-management/:id/accounts` | 关联账号的 ID、名称、平台和类型 |
| `POST /proxy-management/batch` | 按上游批量添加语义新增或跳过；客户端逐项提交并显示结果 |
| `POST /proxy-management/batch-delete` | 返回删除 ID 和逐项拒绝原因 |
| `GET /proxy-management/data` | 按所选 ID 或当前筛选导出 Sub2API JSON，包含认证 |
| `POST /proxy-management/data` | 导入仅含代理的数据包；返回新增、复用及失败明细 |

分页参数 `page`、`page_size`（最多 200）；筛选为 `search`、`protocol`、`status`；排序为 `sort_by=id/name/protocol/account_count` 与 `sort_order=asc/desc`。导出可使用逗号分隔的 `ids`。单次导入、批量添加或删除最多 1000 项，不接受账号数据。导入错误不回传包含认证的 `proxy_key` 或原始错误正文。

编辑支持 IPv6、HTTP/HTTPS/SOCKS5/SOCKS5H、认证、Unix 秒有效期（`null` 清除）、`none/proxy/direct` 到期处理、备用代理和提前提醒天数。未改有效期保留原精度；未改认证不读取或回填密码。普通列表、快照和诊断日志均不包含认证明文。

原生文件操作使用官方 Dialog 与 File System 插件，只给主窗口文件选择和所选文件读写权限。取消选择不读取认证、不提交导入；离线与切换连接会丢弃旧读取结果。所有写请求只发送一次，响应不明时由用户刷新核对。

自 0.1.59 起，管理员主动检测的错误正文、URL、质量目标与消息完整返回，不使用关键词隐藏、凭据替换或短文本截断。客户端按纯文本显示并支持复制；响应 no-store，明文不进入日志。检测响应仍有 1 MiB 传输上限，超过上限明确报错，不返回伪装为完整的截断内容。
