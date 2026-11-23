import AppKit
import SwiftUI
import UniformTypeIdentifiers

extension ProxyManagementTabView {
    func presentModal(_ kind: ModalKind) {
        if let store {
            store.presentWindowModal(onBackdrop: { store.dismissWindowModal() }) {
                modalView(kind, dismiss: { store.dismissWindowModal() })
            }
        } else {
            fallbackModal = kind
        }
    }

    func dismissCurrentModal() {
        if let store {
            store.dismissWindowModal()
        } else {
            fallbackModal = nil
        }
    }

    @ViewBuilder
    func modalView(_ kind: ModalKind, dismiss: @escaping () -> Void) -> some View {
        switch kind {
        case .edit(let row):
            ProxyEditorSheet(
                row: row,
                online: online,
                onSaved: {
                    dismiss()
                    messageText = "已保存"
                    Task { await loadPage() }
                },
                onClose: dismiss
            )
        case .batchAdd:
            ProxyBatchAddSheet(
                online: online,
                onSaved: { msg in
                    dismiss()
                    messageText = msg
                    Task { await loadPage() }
                },
                onClose: dismiss
            )
        case .confirmDelete(let rows):
            LinearModalCard(title: "确认删除 IP", width: 440, onClose: dismiss) {
                Text("确定删除所选的 \(rows.count) 个代理节点吗？关联账号将不再保留此代理。")
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
            } footer: {
                Spacer()
                Button("取消", action: dismiss)
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
                Button("确认删除") {
                    Task { await deleteRows(rows) }
                }
                .buttonStyle(LinearButtonStyle(variant: .solidDanger))
            }
            .sub2opsDialogFocus(onEscape: dismiss)

        case .quality(let row, let q):
            LinearModalCard(
                title: "\(row.name) · 质量检测",
                subtitle: "\(q.grade) · \(Int(q.score.rounded()))/100",
                width: 600,
                onClose: dismiss
            ) {
                VStack(alignment: .leading, spacing: 6) {
                    if !q.summary.isEmpty {
                        DiagnosticTextView(text: q.summary)
                    }
                    let metaParts = [
                        q.exit_ip,
                        q.country,
                        q.base_latency_ms.map { "\(Int($0.rounded())) ms" } ?? "",
                    ].filter { !$0.isEmpty }
                    HStack(spacing: 8) {
                        if !metaParts.isEmpty {
                            Text(metaParts.joined(separator: " · "))
                                .font(DS.fontResetTime)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                        Spacer()
                        if let ts = q.checked_at, ts > 0 {
                            let iso = ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: TimeInterval(ts)))
                            Text(BeijingTime.fullTime(iso))
                                .font(DS.fontResetTime)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                    }
                }

                LinearBoundedScrollView(maxHeight: 280) {
                    VStack(spacing: 0) {
                        HStack(spacing: 8) {
                            Text("目标").frame(width: 112, alignment: .leading)
                            Text("状态").frame(width: 54, alignment: .leading)
                            Text("HTTP").frame(width: 44, alignment: .trailing)
                            Text("延迟").frame(width: 64, alignment: .trailing)
                            Text("详情").frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .font(DS.fontTableHeader)
                        .foregroundStyle(DS.muted)
                        .padding(.horizontal, 10)
                        .frame(height: 28)
                        .background(DS.surfaceSubtle)

                        Rectangle().fill(DS.line).frame(height: 1)

                        ForEach(Array(q.items.enumerated()), id: \.offset) { idx, item in
                            HStack(alignment: .top, spacing: 8) {
                                Text(verbatim: item.target)
                                    .font(DS.fontPriorityMeta).monospacedDigit()
                                    .foregroundStyle(DS.ink)
                                    .frame(width: 112, alignment: .leading)

                                Text(Self.qualityTargetLabels[item.status] ?? item.status)
                                    .font(DS.fontResetTimeSemibold)
                                    .foregroundStyle(item.status == "pass" ? DS.good : DS.warn)
                                    .padding(.horizontal, 5)
                                    .padding(.vertical, 1)
                                    .background(item.status == "pass" ? DS.goodBg : DS.warnBg)
                                    .clipShape(RoundedRectangle(cornerRadius: 4))
                                    .frame(width: 54, alignment: .leading)

                                Text(item.http_status.map(String.init) ?? "—")
                                    .font(DS.fontPriorityMeta).monospacedDigit()
                                    .foregroundStyle(DS.muted)
                                    .frame(width: 44, alignment: .trailing)

                                Text(item.latency_ms.map { "\(Int($0.rounded())) ms" } ?? "—")
                                    .font(DS.fontResetTime)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                                    .frame(width: 64, alignment: .trailing)

                                SwiftUI.Group {
                                    if item.message.isEmpty {
                                        Text("—")
                                            .font(DS.fontCompact)
                                            .foregroundStyle(DS.muted)
                                    } else {
                                        DiagnosticTextView(text: item.message)
                                    }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 7)

                            if idx < q.items.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    }
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                }
            } footer: {
                Spacer()
                Button("关闭", action: dismiss).buttonStyle(LinearButtonStyle(variant: .secondary))
            }
            .sub2opsDialogFocus(onEscape: dismiss)

        case .credentials(let row, let cred):
            LinearModalCard(title: "认证凭据", subtitle: row.name, width: 480, onClose: dismiss) {
                Text(verbatim: row.fullURL(credentials: cred))
                    .font(DS.fontMono12)
                    .foregroundStyle(DS.ink)
                    .textSelection(.enabled)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(DS.surfaceSubtle)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            } footer: {
                Button("复制完整地址") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(row.fullURL(credentials: cred), forType: .string)
                    messageText = "已复制认证地址"
                    dismiss()
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                Spacer()
                Button("关闭", action: dismiss).buttonStyle(LinearButtonStyle(variant: .primary))
            }
            .sub2opsDialogFocus(onEscape: dismiss)

        case .accounts(let row, let list):
            LinearModalCard(title: "关联账号", subtitle: "\(row.name) · \(list.count) 个账号", width: 460, onClose: dismiss) {
                if list.isEmpty {
                    Text("当前无关联账号").font(DS.fontCompact).foregroundStyle(DS.muted)
                } else {
                    LinearBoundedScrollView(maxHeight: 220) {
                        VStack(alignment: .leading, spacing: 6) {
                            ForEach(list) { acc in
                                HStack {
                                    Text(acc.name).font(DS.fontCompactSemibold).foregroundStyle(DS.ink)
                                    Text("#\(acc.id)").font(DS.fontPriorityMeta).monospacedDigit().foregroundStyle(DS.muted)
                                    Spacer()
                                    Text("\(acc.platform) · \(acc.type)").font(DS.fontResetTime).foregroundStyle(DS.muted)
                                }
                            }
                        }
                    }
                }
            } footer: {
                Spacer()
                Button("关闭", action: dismiss).buttonStyle(LinearButtonStyle(variant: .secondary))
            }
            .sub2opsDialogFocus(onEscape: dismiss)

        case .results(let title, let rows):
            LinearModalCard(title: title, width: 500, onClose: dismiss) {
                LinearBoundedScrollView(maxHeight: 240) {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                            VStack(alignment: .leading, spacing: 3) {
                                Text(r.name).font(DS.fontCompactSemibold).foregroundStyle(DS.ink)
                                if r.ok {
                                    Text(r.message)
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(DS.good)
                                } else {
                                    DiagnosticTextView(text: r.message)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
            } footer: {
                Spacer()
                Button("关闭", action: dismiss).buttonStyle(LinearButtonStyle(variant: .secondary))
            }
            .sub2opsDialogFocus(onEscape: dismiss)
        }
    }
}
