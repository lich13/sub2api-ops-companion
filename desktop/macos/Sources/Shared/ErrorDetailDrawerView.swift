import AppKit
import SwiftUI

public struct ErrorDetailDrawerView: View {
    public let error: OpsError?
    public let busy: Bool
    public let onClose: () -> Void

    @State private var copiedId: Bool = false

    public init(error: OpsError?, busy: Bool, onClose: @escaping () -> Void) {
        self.error = error
        self.busy = busy
        self.onClose = onClose
    }

    public var body: some View {
        ZStack(alignment: .trailing) {
            DS.backdrop
                .ignoresSafeArea()
                .onTapGesture(perform: onClose)

            VStack(alignment: .leading, spacing: 0) {
                HStack(alignment: .center, spacing: 10) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("错误详情")
                            .font(DS.fontDialogTitle)
                            .foregroundStyle(DS.ink)
                        if let error {
                            Text(verbatim: "#\(error.id) · \(BeijingTime.fullTime(error.created_at))")
                                .font(DS.fontPriorityMeta)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                    }
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭错误详情", size: 14, action: onClose)
                }
                .padding(.horizontal, 18)
                .padding(.vertical, 14)
                .background(DS.topbar)

                Rectangle().fill(DS.line).frame(height: 1)

                ScrollView {
                    if busy && error == nil {
                        HStack(spacing: 8) {
                            ProgressView().controlSize(.small)
                            Text("正在读取错误原文…")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                        }
                        .padding(22)
                    } else if let error {
                        VStack(alignment: .leading, spacing: 16) {
                            VStack(alignment: .leading, spacing: 8) {
                                metaRow("账号", value: error.account_name.isEmpty ? (error.account_id.map { "#\($0)" } ?? "—") : "\(error.account_name)\(error.account_id.map { " #\($0)" } ?? "")")
                                metaRow("分组", value: error.group_name.isEmpty ? (error.group_id.map { "#\($0)" } ?? "—") : "\(error.group_name)\(error.group_id.map { " #\($0)" } ?? "")")
                                metaRow("HTTP 状态", value: "\(error.status_code)\(error.upstream_status_code.map { "（上游 \($0)）" } ?? "")", mono: true)
                                metaRow("错误码", value: error.provider_error_code.isEmpty ? "—" : error.provider_error_code, mono: true)
                                metaRow("请求模型", value: error.requested_model.isEmpty ? (error.model.isEmpty ? "—" : error.model) : error.requested_model, mono: true)
                                if !error.upstream_model.isEmpty {
                                    metaRow("转发模型", value: error.upstream_model, mono: true)
                                }
                                if let owner = error.error_owner, !owner.isEmpty {
                                    metaRow("来源阶段", value: "\(owner)\(error.error_phase.map { " · \($0)" } ?? "")")
                                }
                                if !error.request_id.isEmpty {
                                    HStack(spacing: 8) {
                                        Text("请求 ID")
                                            .font(DS.fontCompact)
                                            .foregroundStyle(DS.muted)
                                            .frame(width: 76, alignment: .leading)
                                        Text(verbatim: error.request_id)
                                            .font(DS.fontMono11)
                                            .foregroundStyle(DS.ink)
                                            .lineLimit(1)
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
                                    }
                                }
                            }
                            .padding(14)
                            .background(DS.surfaceSubtle)
                            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

                            if !error.message.isEmpty {
                                VStack(alignment: .leading, spacing: 6) {
                                    Text("错误摘要")
                                        .font(DS.fontCompactSemibold)
                                        .foregroundStyle(DS.ink)
                                    DiagnosticTextView(text: error.message)
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
                                            Text("已截断展示")
                                                .font(DS.fontResetTime)
                                                .foregroundStyle(DS.muted)
                                        }
                                    }
                                    DiagnosticTextView(text: content, monospaced: true)
                                }
                            }
                        }
                        .padding(18)
                    }
                }
            }
            .frame(width: 480)
            .frame(maxHeight: .infinity)
            .background(DS.surface)
            .overlay(alignment: .leading) {
                Rectangle().fill(DS.line).frame(width: 1)
            }
            .sub2opsDialogFocus(onEscape: onClose)
        }
    }

    private func metaRow(_ label: String, value: String, mono: Bool = false) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(label)
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(width: 76, alignment: .leading)
            Text(verbatim: value)
                .font(mono ? DS.fontMono11 : DS.fontCompactMedium)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }
}
