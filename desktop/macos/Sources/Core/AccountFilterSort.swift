import CoreGraphics
import Foundation

// MARK: - Account Filtering & Sorting (`desktop/src/types.ts`)

public enum AccountFilterSort {
    public static func filter(
        _ accounts: [Account],
        query: String,
        group: String,
        platform: String,
        status: String,
        type: String = ""
    ) -> [Account] {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let groupId = Int(group)
        return accounts.filter { a in
            let matchesQuery = q.isEmpty || "\(a.id) \(a.name)".lowercased().contains(q)
            let matchesGroup = group.isEmpty || (groupId != nil && a.group_ids.contains(groupId!))
            let matchesPlatform = platform.isEmpty || a.platform == platform
            let matchesType = type.isEmpty || a.type == type
            let matchesStatus: Bool
            if status.isEmpty {
                matchesStatus = true
            } else if status == "ready" {
                matchesStatus = a.available
            } else if status == "error" {
                matchesStatus = a.last_error_id != nil
            } else {
                matchesStatus = !a.schedulable
            }
            return matchesQuery && matchesGroup && matchesPlatform && matchesType && matchesStatus
        }
    }

    public static func sortPriority(_ accounts: [Account], ascending: Bool = true) -> [Account] {
        accounts.sorted { a, b in
            if a.priority != b.priority {
                return ascending ? a.priority < b.priority : a.priority > b.priority
            }
            return a.id < b.id
        }
    }

    public static func sortRecentCall(_ accounts: [Account], ascending: Bool = false) -> [Account] {
        guard accounts.count > 1 else { return accounts }
        let keyed: [(account: Account, timestamp: Double)] = accounts.map { account in
            let recent = BeijingTime.callTimestamp(account.last_called_at)
            let ts = recent.isFinite ? recent : BeijingTime.callTimestamp(account.last_success_at)
            return (account, ts)
        }
        return keyed.sorted { lhs, rhs in
            let left = lhs.timestamp
            let right = rhs.timestamp
            if !left.isFinite {
                return !right.isFinite ? rhs.account.id < lhs.account.id : false
            }
            if !right.isFinite {
                return true
            }
            if left != right {
                return ascending ? left < right : right < left
            }
            return rhs.account.id < lhs.account.id
        }.map(\.account)
    }
}

// MARK: - Quick Panel Geometry & Blur Lifecycle (Migrated from `lib.rs` `PanelLifecycle` & `panel_bounds`)

public struct PanelBlurLifecycle: Sendable {
    public private(set) var generation: UInt64 = 0
    public private(set) var focused: Bool = false
    public var showing: Bool = false

    public init() {}

    public mutating func show(focused: Bool) {
        generation &+= 1
        self.focused = focused
        self.showing = false
    }

    public mutating func focus() {
        generation &+= 1
        self.focused = true
    }

    public mutating func blur() -> UInt64? {
        guard focused && !showing else { return nil }
        focused = false
        return generation
    }

    public func canHide(generation: UInt64) -> Bool {
        generation == self.generation && !focused && !showing
    }
}

public enum PanelGeometry {
    /// Computes the AppKit screen-space frame (origin bottom-left) for the 420pt quick panel,
    /// clamping both width/height (`128...520pt`) and position within `visibleFrame` with 8pt margin.
    public static func panelFrame(
        anchorBottomRight: CGPoint,
        visibleFrame: CGRect,
        contentHeight: CGFloat
    ) -> CGRect {
        let maxW = max(1, visibleFrame.width - 16)
        let maxH = max(1, visibleFrame.height - 16)
        let width = min(DS.quickPanelWidth, maxW)
        let clampedContent = min(max(contentHeight.rounded(.up), DS.quickPanelMinHeight), DS.quickPanelMaxHeight)
        let height = min(clampedContent, maxH)

        let minX = visibleFrame.minX + 8
        let maxX = max(minX, visibleFrame.maxX - width - 8)
        let originX = min(max(anchorBottomRight.x - width + 20, minX), maxX)

        // Position 6pt below the status item bottom edge, clamped inside visibleFrame
        let minY = visibleFrame.minY + 8
        let maxY = max(minY, visibleFrame.maxY - height - 8)
        let originY = min(max(anchorBottomRight.y - height - 6, minY), maxY)

        return CGRect(x: originX.rounded(), y: originY.rounded(), width: width.rounded(), height: height.rounded())
    }

    /// Computes initial centered main window frame (`1310×860` clamped to `visibleFrame` minus 24pt margin).
    public static func mainWindowFrame(visibleFrame: CGRect) -> CGRect {
        let width = min(1310, max(1, visibleFrame.width - 24))
        let height = min(860, max(1, visibleFrame.height - 24))
        let originX = visibleFrame.minX + ((visibleFrame.width - width) / 2.0).rounded()
        let originY = visibleFrame.minY + ((visibleFrame.height - height) / 2.0).rounded()
        return CGRect(x: originX, y: originY, width: width, height: height)
    }
}
