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
    @State private var showPassword: Bool = false
    @State private var usernameDirty: Bool = false
    @State private var clearAuth: Bool = false
    @State private var expiryText: String
    @State private var expiryDirty: Bool = false
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
        self._expiryText = State(initialValue: Self.formatBeijingExpiry(row?.expires_at))
        self._warnDays = State(initialValue: "\(row?.expiry_warn_days ?? 7)")
        self._fallbackMode = State(initialValue: row?.fallback_mode ?? "none")
        self._backupProxyId = State(initialValue: row?.backup_proxy_id ?? 0)
    }

    private static func formatBeijingExpiry(_ raw: String?) -> String {
        guard let raw, let d = BeijingTime.parseISO(raw) else { return "" }
        let df = DateFormatter()
        df.locale = Locale(identifier: "en_US_POSIX")
        df.timeZone = TimeZone(identifier: "Asia/Shanghai") ?? .current
        df.dateFormat = "yyyy-MM-dd HH:mm"
        return df.string(from: d)
    }

    private static func parseBeijingExpiry(_ raw: String) -> Date? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let df = DateFormatter()
        df.locale = Locale(identifier: "en_US_POSIX")
        df.timeZone = TimeZone(identifier: "Asia/Shanghai") ?? .current
        for fmt in ["yyyy-MM-dd HH:mm", "yyyy-MM-dd HH:mm:ss", "yyyy-MM-dd'T'HH:mm", "yyyy-MM-dd'T'HH:mm:ss"] {
            df.dateFormat = fmt
            if let d = df.date(from: trimmed) { return d }
        }
        return nil
    }

    public var body: some View {
        LinearModalCard(
            title: row == nil ? "新增 IP" : "编辑 IP",
            subtitle: row.map { "\($0.name) · #\($0.id)" },
            width: 500,
            closeDisabled: busy,
            onClose: onClose
        ) {
            VStack(alignment: .leading, spacing: 10) {
                labeledField("名称", text: $name, prompt: "节点备注名")

                HStack(spacing: 10) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text("协议").font(DS.fontCompactMedium).foregroundStyle(DS.muted)
                        LinearSelect(
                            label: proto.uppercased(),
                            accessibilityTitle: "协议",
                            height: DS.controlHeight,
                            expandWidth: true,
                            entries: ["http", "https", "socks5", "socks5h"].map { p in
                                LinearMenuEntry(id: p, title: p.uppercased(), checked: proto == p) {
                                    proto = p
                                }
                            }
                        )
                    }

                    if row != nil {
                        VStack(alignment: .leading, spacing: 5) {
                            Text("状态").font(DS.fontCompactMedium).foregroundStyle(DS.muted)
                            LinearSelect(
                                label: status == "active" ? "启用" : "停用",
                                accessibilityTitle: "状态",
                                height: DS.controlHeight,
                                expandWidth: true,
                                entries: [
                                    LinearMenuEntry(id: "active", title: "启用", checked: status == "active") { status = "active" },
                                    LinearMenuEntry(id: "inactive", title: "停用", checked: status == "inactive") { status = "inactive" },
                                ]
                            )
                        }
                    }
                }

                HStack(spacing: 10) {
                    labeledField("主机地址", text: $host, prompt: "proxy.example.invalid")
                    labeledField("端口", text: $port, prompt: "1080", width: 104)
                }

                HStack(spacing: 10) {
                    labeledField(
                        row?.username_set == true ? "用户名（已保存，留空保留）" : "用户名（可选）",
                        text: $username,
                        prompt: "username",
                        mono: true
                    )
                    .onChange(of: username) { _, _ in usernameDirty = true }

                    VStack(alignment: .leading, spacing: 5) {
                        Text(row?.password_set == true ? "密码（留空保留）" : "密码（可选）")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.muted)
                        HStack(spacing: 6) {
                            SwiftUI.Group {
                                if showPassword {
                                    TextField(text: $password, prompt: Text(verbatim: "password")) { EmptyView() }
                                } else {
                                    SecureField(text: $password, prompt: Text(verbatim: "password")) { EmptyView() }
                                }
                            }
                            .textFieldStyle(.plain)
                            .font(DS.fontBody)
                            .foregroundStyle(DS.ink)
                            .disabled(clearAuth)

                            Button {
                                showPassword.toggle()
                            } label: {
                                Image(systemName: showPassword ? "eye.slash" : "eye")
                                    .font(DS.iconFont(12))
                                    .foregroundStyle(DS.muted)
                            }
                            .buttonStyle(.plain)
                            .disabled(clearAuth)
                            .accessibilityLabel(showPassword ? "隐藏密码" : "显示密码")
                        }
                        .padding(.horizontal, 10)
                        .frame(height: DS.controlHeight)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                    }
                }

                if row != nil && (row?.username_set == true || row?.password_set == true) {
                    LinearCheckbox(isChecked: clearAuth, label: "清除已保存的用户名和密码") {
                        clearAuth.toggle()
                    }
                }

                HStack(spacing: 10) {
                    labeledField("到期时间（可选）", text: $expiryText, prompt: "YYYY-MM-DD HH:mm")
                        .onChange(of: expiryText) { _, _ in expiryDirty = true }
                    labeledField("到期提醒天数", text: $warnDays, prompt: "7", width: 112)
                }

                VStack(alignment: .leading, spacing: 5) {
                    Text("到期回退方式").font(DS.fontCompactMedium).foregroundStyle(DS.muted)
                    let fallbackLabel = fallbackMode == "proxy" ? "切换备用代理" : (fallbackMode == "direct" ? "切换直连" : "不切换（保持原代理）")
                    LinearSelect(
                        label: fallbackLabel,
                        accessibilityTitle: "到期回退方式",
                        height: DS.controlHeight,
                        expandWidth: true,
                        entries: [
                            LinearMenuEntry(id: "none", title: "不切换（保持原代理）", checked: fallbackMode == "none") { fallbackMode = "none" },
                            LinearMenuEntry(id: "direct", title: "切换直连", checked: fallbackMode == "direct") { fallbackMode = "direct" },
                            LinearMenuEntry(id: "proxy", title: "切换备用代理", checked: fallbackMode == "proxy") { fallbackMode = "proxy" },
                        ]
                    )
                }

                if fallbackMode == "proxy" {
                    let cur = backupOptions.first(where: { $0.id == backupProxyId })
                    VStack(alignment: .leading, spacing: 5) {
                        Text("备用代理").font(DS.fontCompactMedium).foregroundStyle(DS.muted)
                        LinearSelect(
                            label: cur.map { "\($0.name) (\($0.formattedAddress))" } ?? "请选择备用代理",
                            accessibilityTitle: "备用代理",
                            height: DS.controlHeight,
                            expandWidth: true,
                            mutedLabel: cur == nil,
                            entries: backupOptions.map { opt in
                                LinearMenuEntry(
                                    id: "\(opt.id)",
                                    title: "\(opt.name) (\(opt.formattedAddress))",
                                    checked: backupProxyId == opt.id
                                ) {
                                    backupProxyId = opt.id
                                }
                            }
                        )
                    }
                }
            }
            .disabled(!online || busy)

            if !errorText.isEmpty {
                Text(errorText).font(DS.fontCompact).foregroundStyle(DS.bad)
            }
        } footer: {
            Spacer()
            Button("取消", action: onClose)
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(busy)
            Button(busy ? "保存中…" : (row == nil ? "新增" : "保存")) {
                Task { await submit() }
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
            .disabled(!online || busy)
        }
        .sub2opsDialogFocus(onEscape: { if !busy { onClose() } })
        .task(id: fallbackMode) {
            guard fallbackMode == "proxy", online else { return }
            if let p: ProxyManagedPage = try? await RustBridge.shared.api("GET", "/proxy-management?page=1&page_size=200&status=active") {
                backupOptions = p.items.filter { $0.id != row?.id }
            }
        }
    }

    private func labeledField(_ title: String, text: Binding<String>, prompt: String, mono: Bool = false, width: CGFloat? = nil) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(DS.fontCompactMedium).foregroundStyle(DS.muted)
            TextField(text: text, prompt: Text(verbatim: prompt)) { EmptyView() }
                .textFieldStyle(.plain)
                .font(mono ? DS.fontMono12 : DS.fontBody)
                .foregroundStyle(DS.ink)
                .padding(.horizontal, 10)
                .frame(width: width, height: DS.controlHeight)
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
        let trimmedExpiry = expiryText.trimmingCharacters(in: .whitespacesAndNewlines)
        let parsedExpiry: Date?
        if trimmedExpiry.isEmpty {
            parsedExpiry = nil
        } else {
            guard let d = Self.parseBeijingExpiry(trimmedExpiry) else {
                errorText = "到期时间格式应为 YYYY-MM-DD HH:mm"
                return
            }
            parsedExpiry = d
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
        }
        if row == nil || expiryDirty {
            body["expires_at"] = parsedExpiry.map { .int(Int64(floor($0.timeIntervalSince1970))) } ?? .null
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
        LinearModalCard(
            title: "批量添加 IP",
            subtitle: "每行一条，格式：protocol://[user:pass@]host:port（支持 http / https / socks5 / socks5h）",
            width: 540,
            closeDisabled: busy,
            onClose: onClose
        ) {
            TextEditor(text: $text)
                .font(DS.fontMono12)
                .foregroundStyle(DS.ink)
                .scrollContentBackground(.hidden)
                .frame(height: 156)
                .padding(6)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                .disabled(busy || !results.isEmpty)

            HStack(spacing: 10) {
                Text("有效 \(parsed.proxies.count) 个")
                    .foregroundStyle(DS.ink)
                Text("重复 \(parsed.duplicates) 个")
                    .foregroundStyle(DS.muted)
                if !parsed.errors.isEmpty {
                    Text("第 \(parsed.errors.prefix(8).map(String.init).joined(separator: "、")) 行格式无效")
                        .foregroundStyle(DS.bad)
                }
            }
            .font(DS.fontResetTime)
            .monospacedDigit()

            if !results.isEmpty {
                LinearBoundedScrollView(maxHeight: 160) {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(results.enumerated()), id: \.offset) { _, item in
                            VStack(alignment: .leading, spacing: 3) {
                                Text(verbatim: item.address)
                                    .font(DS.fontPriorityMeta).monospacedDigit()
                                    .foregroundStyle(DS.ink)
                                if item.ok {
                                    Text(item.message)
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(DS.good)
                                } else {
                                    DiagnosticTextView(text: item.message)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
            }
        } footer: {
            Spacer()
            Button(results.isEmpty ? "取消" : "完成") {
                if results.isEmpty {
                    onClose()
                } else {
                    let okCount = results.filter(\.ok).count
                    onSaved("批量添加完成：成功 \(okCount) / \(results.count)")
                }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary))
            .disabled(busy)

            if results.isEmpty {
                Button(busy ? "提交中…" : "开始添加") {
                    Task { await runBatch() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!online || busy || parsed.proxies.isEmpty || !parsed.errors.isEmpty || parsed.proxies.count > 1000)
            }
        }
        .sub2opsDialogFocus(onEscape: { if !busy { onClose() } })
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
