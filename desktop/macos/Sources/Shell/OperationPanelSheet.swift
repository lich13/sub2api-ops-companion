import SwiftUI

/// Matches `desktop/src/OperationPanel.tsx` (`AccountOperations` dialog).
public struct OperationToolbarButton: View {
    @Bindable public var store: AppStore

    public init(store: AppStore) {
        self.store = store
    }

    public var body: some View {
        let ops = store.operations
        let pendingCount = ops.pendingOperations.count
        if !ops.rows.isEmpty || !ops.error.isEmpty {
            Button {
                ops.isOpen = true
                ops.startPollingIfNeeded(online: store.state.online, active: true)
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "checklist")
                        .font(DS.iconFont(13, weight: .medium))
                    if pendingCount > 0 {
                        Text("\(pendingCount)")
                            .font(DS.fontBadge)
                            .monospacedDigit()
                            .foregroundStyle(DS.accent)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 1)
                            .background(DS.accentBg)
                            .clipShape(Capsule())
                    }
                }
                .foregroundStyle(DS.muted)
                .padding(.horizontal, 7)
                .frame(height: 28)
            }
            .buttonStyle(.linearPlain)
            .help("操作待办 \(pendingCount)")
            .accessibilityLabel("操作待办 \(pendingCount)")
            .popover(isPresented: Bindable(ops).isOpen, arrowEdge: .bottom) {
                OperationPanelSheet(store: store)
            }
        }
    }
}

public struct OperationPanelSheet: View {
    @Bindable public var store: AppStore

    private static let stateLabels: [String: String] = [
        "queued": "已排队",
        "running": "执行中",
        "checking": "核对中",
        "completed": "已完成",
        "needs_confirmation": "需确认",
        "failed": "失败",
        "cancelled": "已取消",
        "superseded": "已替代",
    ]

    private static let actionLabels: [String: String] = [
        "concurrency": "并发",
        "load_factor": "负载因子",
        "model_config": "模型配置",
        "proxy": "选择代理",
        "sync_models": "同步上游模型",
        "priority": "优先级",
        "groups": "分组",
        "recover": "恢复状态",
        "usage": "额度操作",
        "reset_quota": "使用重置卡",
        "delete": "删除账号",
        "test": "连接测试",
        "schedulable": "调度",
        "degradation_mark": "降智标记",
        "model_test": "模型测试",
        "account_template": "应用模板",
    ]

    public init(store: AppStore) {
        self.store = store
    }

    public var body: some View {
        let ops = store.operations
        let displayed = ops.batchId != nil ? ops.batchItems : ops.rows

        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("操作待办")
                    .font(DS.fontSectionTitle)
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭待办") {
                    ops.isOpen = false
                }
            }

            if ops.batchId != nil {
                Button("返回全部待办") {
                    ops.batchId = nil
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            }

            if !ops.error.isEmpty {
                Text(ops.error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }

            Divider().overlay(DS.line)

            LinearBoundedScrollView(maxHeight: 420) {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(displayed) { job in
                        operationRow(job)
                    }
                    if displayed.isEmpty {
                        Text("暂无操作记录")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                            .frame(maxWidth: .infinity, alignment: .center)
                            .padding(.vertical, 20)
                    }
                }
            }
        }
        .padding(16)
        .frame(width: 440)
        .background(DS.surface)
        .sub2opsDialogFocus {
            if ops.batchId != nil {
                ops.batchId = nil
            } else {
                ops.isOpen = false
            }
        }
    }

    @ViewBuilder
    private func operationRow(_ job: Operation) -> some View {
        let ops = store.operations
        let needsConfirm = job.status == "needs_confirmation" || job.repeat_state == "needs_confirmation"
        let statusText: String = {
            if job.repeat_state == "needs_confirmation" { return "需确认" }
            if job.repeat_state == "stopped" { return "已停止" }
            if job.repeat_state == "active" && job.status == "completed" { return "准备下一次预约" }
            return Self.stateLabels[job.status] ?? job.status
        }()
        let actionTitle = job.repeat_mode != nil ? "持续预约测试" : (Self.actionLabels[job.action] ?? job.action)

        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline) {
                Text(job.accountNameText)
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Text("#\(job.account_id)")
                    .font(DS.fontPriorityMeta)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                Spacer()
                Text("\(actionTitle) · \(statusText)")
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(needsConfirm || job.status == "failed" ? DS.bad : DS.muted)
            }

            if let reason = job.repeat_reason ?? job.reason, !reason.isEmpty {
                Text(reason)
                    .font(DS.fontCompact)
                    .foregroundStyle(needsConfirm || job.status == "failed" ? DS.bad : DS.muted)
            }

            if let scheduled = job.scheduled_at {
                Text(BeijingTime.fullTime(scheduled))
                    .font(DS.fontPriorityMeta)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            }

            if let attempts = job.attempt_count {
                let retrySuffix = job.next_retry_at.map { " · 下次重试 \(BeijingTime.fullTime($0))" } ?? ""
                Text("已尝试 \(attempts) 次\(retrySuffix)")
                    .font(DS.fontPriorityMeta)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            }

            if needsConfirm {
                VStack(alignment: .leading, spacing: 4) {
                    Text("请求：\(Self.describePayload(job.requested.asPayload(for: job.action)))")
                        .font(DS.fontPriorityMeta)
                        .foregroundStyle(DS.ink)
                    if let current = job.current {
                        Text("当前：\(Self.describePayload(current.asPayload(for: job.action)))")
                            .font(DS.fontPriorityMeta)
                            .foregroundStyle(DS.muted)
                    }
                }
                .padding(8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(DS.surfaceSubtle)
                .clipShape(RoundedRectangle(cornerRadius: 6))
            }

            HStack(spacing: 8) {
                if ops.batchId == nil, let bId = job.batch_id {
                    Button("查看整批") {
                        ops.batchItems = []
                        ops.batchId = bId
                        Task { await ops.pollOnce() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }

                if needsConfirm {
                    let retryLabel: String = {
                        if job.repeat_mode != nil { return "重新预约" }
                        if job.action == "priority" { return "进入优先级页核对" }
                        if job.action == "account_template" { return "重新预览模板" }
                        return "确认后重新提交"
                    }()
                    Button(retryLabel) {
                        Task { await retryJob(job) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    .disabled(!store.state.online)
                }

                if OperationStore.isRepeating(job) {
                    Button("停止连续预约") {
                        Task {
                            do {
                                _ = try await ops.cancelJob(job)
                            } catch {
                                store.report(error)
                            }
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!store.state.online)
                } else if canCancelSingle(job) {
                    Button("取消") {
                        Task {
                            do {
                                _ = try await ops.cancelJob(job)
                            } catch {
                                store.report(error)
                            }
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!store.state.online)
                }
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(DS.surfaceSubtle)
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private func canCancelSingle(_ job: Operation) -> Bool {
        if job.status != "queued" && job.action == "test" && (job.batch_id != nil || job.scheduled_at != nil) && job.started_at != nil {
            return false
        }
        if job.status == "queued" || job.status == "needs_confirmation" {
            return true
        }
        return job.status == "running" && (job.action == "test" || job.action == "model_test")
    }

    private func retryJob(_ job: Operation) async {
        let ops = store.operations
        if job.action == "priority" {
            ops.isOpen = false
            store.page = .priorities
            return
        }
        guard let account = store.accounts.first(where: { $0.id == job.account_id }) else {
            store.showToast("账号已删除或无法读取")
            return
        }
        if job.action == "account_template" {
            ops.isOpen = false
            store.onOpenTemplates?(account)
            return
        }
        if job.action == "model_test" {
            ops.isOpen = false
            store.onOpenModelTest?(account)
            return
        }
        do {
            let capturedEpoch = ops.epoch
            if job.repeat_mode != nil {
                try await ops.cancelJob(job)
                guard ops.epoch == capturedEpoch else { return }
            }
            _ = try await ops.submitOperation(
                account: account,
                action: job.action,
                payload: job.requested.asPayload(for: job.action),
                repeatMode: job.repeat_mode
            )
            store.refresh()
        } catch {
            store.report(error)
        }
    }

    public static func describePayload(_ value: [String: JSONValue]) -> String {
        if let whitelist = value["whitelist"]?.arrayValue, let mappings = value["mappings"]?.arrayValue {
            let w = whitelist.compactMap(\.stringValue)
            let m = mappings.compactMap { item -> String? in
                guard let src = item["source"]?.stringValue, let dst = item["target"]?.stringValue else { return nil }
                return "\(src) → \(dst)"
            }
            let combined = (w + m).joined(separator: "、")
            return combined.isEmpty ? "不限制模型" : combined
        }
        if let proxyVal = value["proxy_id"] {
            if proxyVal == .null { return "不使用代理" }
            return "代理 #\(proxyVal.displayText)"
        }
        if let lf = value["load_factor"] {
            return "负载因子：\(lf == .null ? "自动" : lf.displayText)"
        }
        let names: [(String, String)] = [
            ("priority", "优先级"),
            ("group_ids", "目标分组"),
            ("scope_group_ids", "编辑分组"),
            ("schedulable", "调度"),
            ("marked", "降智标记"),
            ("model_id", "模型"),
            ("concurrency", "并发"),
            ("status", "账号状态"),
            ("name", "名称"),
            ("type", "类型"),
            ("action", "操作"),
        ]
        var parts: [String] = []
        for (key, label) in names {
            guard let v = value[key] else { continue }
            if let arr = v.arrayValue {
                let ids = arr.map { "#\($0.displayText)" }.joined(separator: "、")
                parts.append("\(label)：\(ids.isEmpty ? "无" : ids)")
            } else {
                parts.append("\(label)：\(v.displayText)")
            }
        }
        return parts.isEmpty ? "核对账号状态" : parts.joined(separator: "；")
    }
}
