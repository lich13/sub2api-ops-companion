import SwiftUI

public struct ConnectionBatchBanner: View {
    public let batch: OperationBatch?
    public let onOpenBatchDetails: (String) -> Void
    public let onDismiss: () -> Void

    public init(
        batch: OperationBatch?,
        onOpenBatchDetails: @escaping (String) -> Void,
        onDismiss: @escaping () -> Void
    ) {
        self.batch = batch
        self.onOpenBatchDetails = onOpenBatchDetails
        self.onDismiss = onDismiss
    }

    public var body: some View {
        if let batch, !batch.items.isEmpty {
            let doneCount = batch.items.filter { OperationStore.isTerminal($0.status) }.count
            let okCount = batch.items.filter { $0.status == "completed" }.count
            let failCount = doneCount - okCount
            let finished = doneCount == batch.items.count
            HStack(spacing: 10) {
                if !finished {
                    ProgressView().controlSize(.small)
                } else {
                    StatusDot(failCount == 0 ? .good : .warning)
                }
                Text(
                    finished
                        ? "批量测活完成 · 成功 \(okCount) / \(batch.items.count)\(failCount > 0 ? " · 异常 \(failCount)" : "")"
                        : "批量测活进行中 · \(doneCount) / \(batch.items.count)"
                )
                .font(DS.fontCompactMedium)
                .monospacedDigit()
                .foregroundStyle(DS.ink)

                Spacer()

                Button("查看详情") {
                    onOpenBatchDetails(batch.batch_id)
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))

                LinearIconButton(systemName: "xmark", label: "收起测活状态", size: 11, action: onDismiss)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            .task(id: "\(batch.batch_id):\(finished)") {
                guard finished && failCount == 0 else { return }
                try? await Task.sleep(for: .seconds(3))
                guard !Task.isCancelled else { return }
                onDismiss()
            }
        }
    }
}
