import AppKit
import SwiftUI

/// Matches `desktop/src/DiagnosticText.tsx`: displays complete verbatim diagnostic text
/// without truncation or masking, with an explicit copy button and status feedback.
public struct DiagnosticTextView: View {
    public let text: String
    public var monospaced: Bool
    @State private var copyStatus: String = ""

    public init(text: String, monospaced: Bool = false) {
        self.text = text
        self.monospaced = monospaced
    }

    public var body: some View {
        if !text.isEmpty {
            HStack(alignment: .top, spacing: 6) {
                Text(verbatim: text)
                    .font(monospaced ? DS.fontMono11 : DS.fontCompact)
                    .foregroundStyle(DS.ink)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                Button {
                    let board = NSPasteboard.general
                    board.clearContents()
                    if board.setString(text, forType: .string) {
                        copyStatus = "已复制"
                    } else {
                        copyStatus = "复制失败，可选择文本复制"
                    }
                } label: {
                    Image(systemName: "doc.on.doc")
                        .font(DS.iconFont(12))
                        .foregroundStyle(DS.muted)
                }
                .buttonStyle(.plain)
                .help("复制完整检测详情")
                .accessibilityLabel("复制完整检测详情")

                if !copyStatus.isEmpty {
                    Text(copyStatus)
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
            }
        }
    }
}
