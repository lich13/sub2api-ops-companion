import SwiftUI

/// Dialog sheets for IP/Proxy management (`ProxyDialogs.tsx`).
public struct ProxyEditorSheet: View {
    public let row: ProxyManagedRow?
    public let online: Bool
    public let onSaved: () -> Void
    public let onClose: () -> Void

    @State private var name: String
    @State private var proto: String
    @State private var host: String
    @State private var port: String
    @State private var status: String
    @State private var username: String = ""
    @State private var password: String = ""
    @State private var usernameDirty: Bool = false
    @State private var clearAuth: Bool = false
    @State private var warnDays: String
    @State private var fallbackMode: String
    @State private var backupProxyId: Int = 0
    @State private var backupOptions: [ProxyManagedRow] = []
    @State private var busy: Bool = false
    @State private var errorText: String = ""

    public init(row: ProxyManagedRow?, online: Bool, onSaved: @escaping () -> Void, onClose: @escaping () -> Void) {
        self.row = row
        self.online = online
        self.onSaved = onSaved
        self.onClose = onClose
        self._name = State(initialValue: row?.name ?? "")
        self._proto = State(initialValue: row?.protocolName ?? "socks5")
        self._host = State(initialValue: row?.host ?? "")
        self._port = State(initialValue: "\(row?.port ?? 1080)")
        self._status = State(initialValue: row?.status == "inactive" ? "inactive" : "active")
        self._warnDays = State(initialValue: "\(row?.expiry_warn_days ?? 7)")
        self._fallbackMode = State(initialValue: row?.fallback_mode ?? "none")
        self._backupProxyId = State(initialValue: row?.backup_proxy_id ?? 0)
    }

    public var body: some View {
        ZStack {
            DS.backdrop.ignoresSafeArea().onTapGesture { if !busy { onClose() } }
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Text(row == nil ? "新增 IP" : "编辑 IP")
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭", disabled: busy, action: onClose)
                }

                VStack(alignment: .leading, spacing: 10) {
                    labeledField("名称", text: $name, prompt: "节点备注名")
                    HStack(spacing: 10) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("协议").font(DS.fontCompactMedium).foregroundStyle(DS.ink)
                            Menu {
                                ForEach(["http", "https", "socks5", "socks5h"], id: \.self) { p in
                                    Button(p.uppercased()) { proto = p }
                                }
                            } label: {
                                menuBox(proto.uppercased())
                            }
                            .menuStyle(.borderlessButton)
                            .menuIndicator(.hidden)
                        }
                        if row != nil {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("状态").font(DS.fontCompactMedium).foregroundStyle(DS.ink)
                                Menu {
                                    Button("启用") { status = "active" }
                                    Button("停用") { status = "inactive" }
                                } label: {
                                    menuBox(status == "active" ? "启用" : "停用")
                                }
                                .menuStyle(.borderlessButton)
                                .menuIndicator(.hidden)
                            .menuIndicator(.hidden)
                            }
                        }
                    }

                    HStack(spacing: 10) {
                        labeledField("主机地址", text: $host, prompt: "proxy.example.invalid", mono: true)
                        labeledField("端口", text: $port, prompt: "1080", mono: true, width: 96)
                    }

                    HStack(spacing: 10) {
                        labeledField(
                            row?.username_set == true ? "用户名（已保存，留空保留）" : "用户名（可选）",
                            text: Binding(get: { username }, set: { username = $0; usernameDirty = true }),
                            prompt: "username",
                            mono: true
                        )
                        VStack(alignment: .leading, spacing: 4) {
                            Text(row?.password_set == true ? "密码（留空保留）" : "密码（可选）")
                                .font(DS.fontCompactMedium)
                                .foregroundStyle(DS.ink)
                            SecureField(text: $password, prompt: Text(verbatim: "password")) { EmptyView() }
                                .textFieldStyle(.plain)
                                .font(DS.fontMono12)
                                .padding(.horizontal, 10)
                                .frame(height: DS.controlHeightDense)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                        }
                    }

                    if row?.username_set == true || row?.password_set == true {
                        Button {
                            clearAuth.toggle()
                        } label: {
                            HStack(spacing: 6) {
                                Image(systemName: clearAuth ? "checkmark.square.fill" : "square")
                                    .font(DS.iconFont(13))
                                    .foregroundStyle(clearAuth ? DS.accent : DS.muted)
                                Text("清除已保存的认证凭据")
                                    .font(DS.fontCompact)
                                    .foregroundStyle(DS.ink)
                            }
                        }
                        .buttonStyle(.plain)
                    }

                    HStack(spacing: 10) {
                        labeledField("到期提醒天数", text: $warnDays, prompt: "7", mono: true, width: 120)
                        VStack(alignment: .leading, spacing: 4) {
                            Text("到期回退方式").font(DS.fontCompactMedium).foregroundStyle(DS.ink)
                            Menu {
                                Button("不切换（保持原代理）") { fallbackMode = "none" }
                                Button("切换直连") { fallbackMode = "direct" }
                                Button("切换备用代理") { fallbackMode = "proxy" }
                            } label: {
                                let labelText = fallbackMode == "proxy" ? "切换备用代理" : (fallbackMode == "direct" ? "切换直连" : "不切换")
                                menuBox(labelText)
                            }
                            .menuStyle(.borderlessButton)
                            .menuIndicator(.hidden)
                        }
                    }

                    if fallbackMode == "proxy" {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("备用代理").font(DS.fontCompactMedium).foregroundStyle(DS.ink)
                            Menu {
                                ForEach(backupOptions) { opt in
                                    Button("\(opt.name) (\(opt.formattedAddress))") { backupProxyId = opt.id }
                                }
                            } label: {
                                let cur = backupOptions.first(where: { $0.id == backupProxyId })
                                menuBox(cur.map { "\($0.name) (\($0.formattedAddress))" } ?? "请选择备用代理")
                            }
                            .menuStyle(.borderlessButton)
                            .menuIndicator(.hidden)
                        }
                    }
                }
                .disabled(!online || busy)

                if !errorText.isEmpty {
                    Text(errorText).font(DS.fontCompact).foregroundStyle(DS.bad)
                }

                HStack {
                    Spacer()
                    Button("取消", action: onClose)
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(busy)
                    Button(busy ? "保存中…" : (row == nil ? "新增" : "保存")) {
                        Task { await submit() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    .disabled(!online || busy)
                }
            }
            .padding(20)
            .frame(width: 480)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .sub2opsDialogFocus(onEscape: { if !busy { onClose() } })
            .task(id: fallbackMode) {
                guard fallbackMode == "proxy", online else { return }
                if let p: ProxyManagedPage = try? await RustBridge.shared.api("GET", "/proxy-management?page=1&page_size=200&status=active") {
                    backupOptions = p.items.filter { $0.id != row?.id }
                }
            }
        }
    }

    private func menuBox(_ title: String) -> some View {
        HStack {
            Text(title).font(DS.fontCompact).foregroundStyle(DS.ink).lineLimit(1)
            Spacer()
            Image(systemName: "chevron.down").font(DS.iconFont(10)).foregroundStyle(DS.muted)
        }
        .padding(.horizontal, 10)
        .frame(height: DS.controlHeightDense)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
    }

    private func labeledField(_ title: String, text: Binding<String>, prompt: String, mono: Bool = false, width: CGFloat? = nil) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(DS.fontCompactMedium).foregroundStyle(DS.ink)
            TextField(text: text, prompt: Text(verbatim: prompt)) { EmptyView() }
                .textFieldStyle(.plain)
                .font(mono ? DS.fontMono12 : DS.fontBody)
                .padding(.horizontal, 10)
                .frame(width: width, height: DS.controlHeightDense)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
    }

    private func submit() async {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedHost = host.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "^\\[|\\]$", with: "", options: .regularExpression)
        guard !trimmedName.isEmpty, !trimmedHost.isEmpty,
              let portInt = Int(port), (1...65535).contains(portInt),
              let warnInt = Int(warnDays), warnInt >= 0
        else {
            errorText = "请填写名称、地址、有效端口和非负提醒天数"
            return
        }
        if fallbackMode == "proxy" && (backupProxyId <= 0 || backupProxyId == row?.id) {
            errorText = "请选择备用代理"
            return
        }
        var body: [String: JSONValue] = [
            "name": .string(trimmedName),
            "protocol": .string(proto),
            "host": .string(trimmedHost),
            "port": .int(Int64(portInt)),
            "fallback_mode": .string(fallbackMode),
            "backup_proxy_id": fallbackMode == "proxy" ? .int(Int64(backupProxyId)) : .null,
            "expiry_warn_days": .int(Int64(warnInt)),
        ]
        if row != nil {
            body["status"] = .string(status)
        } else {
            body["expires_at"] = .null
        }
        if clearAuth {
            body["username"] = .string("")
            body["password"] = .string("")
        } else {
            if row == nil || usernameDirty { body["username"] = .string(username) }
            if row == nil || !password.isEmpty { body["password"] = .string(password) }
        }
        busy = true
        errorText = ""
        defer { busy = false }
        do {
            let _: JSONValue = try await RustBridge.shared.api(
                row == nil ? "POST" : "PUT",
                row.map { "/proxy-management/\($0.id)" } ?? "/proxy-management",
                body: .object(body)
            )
            onSaved()
        } catch {
            errorText = error.localizedDescription
        }
    }
}

public struct ProxyBatchAddSheet: View {
    public let online: Bool
    public let onSaved: (String) -> Void
    public let onClose: () -> Void

    @State private var text: String = ""
    @State private var busy: Bool = false
    @State private var results: [(address: String, ok: Bool, message: String)] = []

    public init(online: Bool, onSaved: @escaping (String) -> Void, onClose: @escaping () -> Void) {
        self.online = online
        self.onSaved = onSaved
        self.onClose = onClose
    }

    private var parsed: ProxyLineParseOutput {
        ProxyDataParser.parseProxyLines(text)
    }

    public var body: some View {
        ZStack {
            DS.backdrop.ignoresSafeArea().onTapGesture { if !busy { onClose() } }
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("批量添加 IP")
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭", disabled: busy, action: onClose)
                }

                Text("每行一条，格式：protocol://[user:pass@]host:port（支持 http / https / socks5 / socks5h）")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)

                TextEditor(text: $text)
                    .font(DS.fontMono12)
                    .frame(height: 150)
                    .padding(6)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                    .disabled(busy || !results.isEmpty)

                HStack(spacing: 10) {
                    Text("识别 \(parsed.proxies.count) 条")
                        .foregroundStyle(DS.ink)
                    if parsed.duplicates > 0 {
                        Text("去重 \(parsed.duplicates) 条").foregroundStyle(DS.muted)
                    }
                    if !parsed.errors.isEmpty {
                        Text("格式错误行：\(parsed.errors.prefix(8).map(String.init).joined(separator: ", "))")
                            .foregroundStyle(DS.bad)
                    }
                }
                .font(DS.fontResetTime)
                .monospacedDigit()

                if !results.isEmpty {
                    ScrollView {
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(Array(results.enumerated()), id: \.offset) { _, item in
                                HStack {
                                    Text(verbatim: item.address).font(DS.fontMono11).foregroundStyle(DS.ink)
                                    Spacer()
                                    Text(item.message)
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(item.ok ? DS.good : DS.bad)
                                }
                            }
                        }
                    }
                    .frame(maxHeight: 130)
                }

                HStack {
                    Spacer()
                    Button(results.isEmpty ? "取消" : "完成") {
                        if results.isEmpty {
                            onClose()
                        } else {
                            let okCount = results.filter(\.ok).count
                            onSaved("批量添加完成：成功 \(okCount) / \(results.count)")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(busy)

                    if results.isEmpty {
                        Button(busy ? "提交中…" : "开始添加") {
                            Task { await runBatch() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                        .disabled(!online || busy || parsed.proxies.isEmpty || !parsed.errors.isEmpty || parsed.proxies.count > 1000)
                    }
                }
            }
            .padding(20)
            .frame(width: 520)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .sub2opsDialogFocus(onEscape: { if !busy { onClose() } })
        }
    }

    private func runBatch() async {
        struct BatchReply: Decodable, Sendable { var created: Int; var skipped: Int }
        let items = parsed.proxies
        busy = true
        defer { busy = false }
        var out: [(address: String, ok: Bool, message: String)] = []
        for item in items {
            guard online else { break }
            do {
                let r: BatchReply = try await RustBridge.shared.api(
                    "POST",
                    "/proxy-management/batch",
                    body: .object(["proxies": .array([item.jsonObject])])
                )
                let ok = r.created == 1
                out.append((item.displayAddress, ok, ok ? "已添加" : "已存在或未添加"))
            } catch {
                out.append((item.displayAddress, false, error.localizedDescription))
            }
            results = out
        }
    }
}
