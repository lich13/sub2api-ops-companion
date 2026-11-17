import AppKit
import SwiftUI

/// Matches right-side `.drawer` error detail in `desktop/src/main.tsx` (including DegradationMark action per DESIGN.md line 45).
public struct ErrorDetailDrawerView: View {
    public let error: OpsError?
    public let busy: Bool
    public var account: Account?
    public var store: AppStore?
    public let onClose: () -> Void

    @State private var copiedId: Bool = false

    public init(
        error: OpsError?,
        busy: Bool,
        account: Account? = nil,
        store: AppStore? = nil,
        onClose: @escaping () -> Void
    ) {
        self.error = error
        self.busy = busy
        self.account = account
        self.store = store
        self.onClose = onClose
    }

    public var body: some View {
        ZStack(alignment: .trailing) {
            DS.backdrop
                .ignoresSafeArea()
                .onTapGesture(perform: onClose)

            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .top, spacing: 12) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text(error.map { "HTTP \($0.upstream_status_code ?? $0.status_code) · 错误详情" } ?? "错误详情")
                            .font(DS.fontPriorityMeta)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                        Text(error?.account_name.isEmpty == false ? error!.account_name : (error?.account_id.map { "账号 #\($0)" } ?? "读取中…"))
                            .font(DS.fontDialogTitle)
                            .foregroundStyle(DS.ink)
                    }
                    Spacer()
                    if let account, let store, account.platform == "openai", ["oauth", "apikey"].contains(account.type) {
                        let marked = account.degradation_mark?.marked == true
                        Button(marked ? "取消标记" : "标记降智") {
                            Task { await DegradationMarkActions.toggle(account: account, store: store) }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(!store.state.online || account.degradation_mark?.version == nil)
                    }
                    LinearIconButton(systemName: "xmark", label: "关闭错误详情", size: 13, action: onClose)
                }
                .padding(.horizontal, 24)
                .padding(.vertical, 18)

                Rectangle().fill(DS.line).frame(height: 1)

                ScrollView {
                    if busy && error == nil {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text("正在读取错误原文…")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                        }
                        .padding(24)
                    } else if let error {
                        VStack(alignment: .leading, spacing: 18) {
                            HStack(alignment: .top, spacing: 10) {
                                Image(systemName: "exclamationmark.circle")
                                    .font(DS.iconFont(15, weight: .semibold))
                                    .foregroundStyle(DS.bad)
                                    .padding(.top, 1)
                                VStack(alignment: .leading, spacing: 5) {
                                    Text(verbatim: error.provider_error_code.isEmpty ? "HTTP \(error.upstream_status_code ?? error.status_code)" : error.provider_error_code)
                                        .font(DS.fontBodySemibold)
                                        .foregroundStyle(DS.bad)
                                    if !error.message.isEmpty {
                                        Text(verbatim: error.message)
                                            .font(DS.fontCompact)
                                            .foregroundStyle(DS.bad)
                                            .textSelection(.enabled)
                                    }
                                }
                                Spacer(minLength: 0)
                            }
                            .padding(14)
                            .background(DS.badBg)
                            .clipShape(RoundedRectangle(cornerRadius: 8))

                            VStack(alignment: .leading, spacing: 10) {
                                metaRow("发生时间", value: BeijingTime.fullTime(error.created_at))
                                metaRow("关联账号", value: error.account_name.isEmpty ? (error.account_id.map { "#\($0)" } ?? "—") : "\(error.account_name)\(error.account_id.map { " #\($0)" } ?? "")")
                                metaRow("所属分组", value: error.group_name.isEmpty ? (error.group_id.map { "#\($0)" } ?? "—") : "\(error.group_name)\(error.group_id.map { " #\($0)" } ?? "")")
                                metaRow("HTTP 状态", value: "\(error.status_code)\(error.upstream_status_code.map { "（上游 \($0)）" } ?? "")")
                                metaRow("请求模型", value: error.requested_model.isEmpty ? (error.model.isEmpty ? "—" : error.model) : error.requested_model)
                                if !error.upstream_model.isEmpty {
                                    metaRow("转发模型", value: error.upstream_model)
                                }
                                if let owner = error.error_owner, !owner.isEmpty {
                                    metaRow("来源阶段", value: "\(owner)\(error.error_phase.map { " · \($0)" } ?? "")")
                                }
                                if !error.request_id.isEmpty {
                                    HStack(spacing: 10) {
                                        Text("请求 ID")
                                            .font(DS.fontCompact)
                                            .foregroundStyle(DS.muted)
                                            .frame(width: 82, alignment: .leading)
                                        Text(verbatim: error.request_id)
                                            .font(DS.fontMono11)
                                            .foregroundStyle(DS.ink)
                                            .lineLimit(1)
                                            .truncationMode(.middle)
                                        Button {
                                            NSPasteboard.general.clearContents()
                                            NSPasteboard.general.setString(error.request_id, forType: .string)
                                            copiedId = true
                                        } label: {
                                            Image(systemName: copiedId ? "checkmark" : "doc.on.doc")
                                                .font(DS.iconFont(11))
                                                .foregroundStyle(copiedId ? DS.good : DS.muted)
                                        }
                                        .buttonStyle(.plain)
                                        .help("复制请求 ID")
                                        Spacer(minLength: 0)
                                    }
                                }
                            }

                            if let content = error.content, !content.isEmpty {
                                VStack(alignment: .leading, spacing: 6) {
                                    HStack {
                                        Text("上游响应原文")
                                            .font(DS.fontCompactSemibold)
                                            .foregroundStyle(DS.ink)
                                        Spacer()
                                        if error.content_limited == true {
                                            Text("内容较长已截断")
                                                .font(DS.fontResetTime)
                                                .foregroundStyle(DS.warn)
                                        }
                                    }
                                    DiagnosticTextView(text: content, monospaced: true)
                                }
                            }
                        }
                        .padding(24)
                    }
                }
            }
            .frame(width: 480)
            .frame(maxHeight: .infinity)
            .background(DS.surface)
            .overlay(alignment: .leading) {
                Rectangle().fill(DS.line).frame(width: 1)
            }
            .shadow(color: DS.modalShadowColor, radius: 24, x: -8, y: 0)
            .sub2opsDialogFocus(onEscape: onClose)
        }
    }

    private func metaRow(_ label: String, value: String, mono: Bool = false) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(label)
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(width: 82, alignment: .leading)
            Text(verbatim: value)
                .font(mono ? DS.fontMono11 : DS.fontCompactMedium)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }
}
