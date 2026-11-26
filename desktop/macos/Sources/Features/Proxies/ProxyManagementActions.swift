import AppKit
import SwiftUI
import UniformTypeIdentifiers

extension ProxyManagementTabView {
    private func buildQueryString(exporting: Bool = false) -> String {
        var comps = URLComponents()
        var items: [URLQueryItem] = [
            URLQueryItem(name: "sort_by", value: sortBy),
            URLQueryItem(name: "sort_order", value: sortOrder),
        ]
        if !exporting {
            items.append(URLQueryItem(name: "page", value: "\(page)"))
            items.append(URLQueryItem(name: "page_size", value: "\(pageSize)"))
        }
        let q = search.trimmingCharacters(in: .whitespacesAndNewlines)
        if !q.isEmpty { items.append(URLQueryItem(name: "search", value: q)) }
        if !protocolFilter.isEmpty { items.append(URLQueryItem(name: "protocol", value: protocolFilter)) }
        if !statusFilter.isEmpty { items.append(URLQueryItem(name: "status", value: statusFilter)) }
        comps.queryItems = items
        return comps.percentEncodedQuery ?? ""
    }

    func loadPage() async {
        guard online else { return }
        loading = true
        errorText = ""
        defer { loading = false }
        do {
            let r: ProxyManagedPage = try await RustBridge.shared.api("GET", "/proxy-management?\(buildQueryString())")
            if r.pages > 0 && page > r.pages {
                page = r.pages
                return
            }
            pageData = r
            selectedIds = selectedIds.intersection(Set(r.items.map(\.id)))
        } catch {
            errorText = error.localizedDescription
        }
    }

    func toggleRowStatus(_ row: ProxyManagedRow) async {
        guard online, busyLabel.isEmpty else { return }
        busyLabel = "status"
        defer { busyLabel = "" }
        do {
            let nextStatus = row.status == "active" ? "inactive" : "active"
            try await RustBridge.shared.apiVoid(
                "PUT",
                "/proxy-management/\(row.id)",
                body: .object(["status": .string(nextStatus)])
            )
            messageText = "状态已更新"
            await loadPage()
            store?.refresh()
        } catch {
            errorText = error.localizedDescription
        }
    }

    func runDiagnose(rows: [ProxyManagedRow], quality: Bool) async {
        guard online, busyLabel.isEmpty else { return }
        busyLabel = quality ? "quality" : "test"
        defer { busyLabel = "" }
        var out: [(name: String, ok: Bool, message: String)] = []
        for row in rows {
            do {
                if quality {
                    let q: ProxyQualityResult = try await RustBridge.shared.api("POST", "/proxies/\(row.id)/quality-check")
                    qualityById[row.id] = q
                    out.append((row.name, true, "\(q.grade) · \(Int(q.score.rounded()))/100 \(q.summary)"))
                    if rows.count == 1 {
                        presentModal(.quality(row, q))
                        await loadPage()
                        return
                    }
                } else {
                    let c: ProxyTestReply = try await RustBridge.shared.api("POST", "/proxies/\(row.id)/test")
                    out.append((row.name, c.success, c.success ? "连接成功\(c.latency_ms.map { " · \(Int($0.rounded())) ms" } ?? "")" : (c.message.isEmpty ? "连接失败" : c.message)))
                }
            } catch {
                out.append((row.name, false, error.localizedDescription))
            }
        }
        if rows.count > 1 || out.contains(where: { !$0.ok }) {
            presentModal(.results(quality ? "质量检测结果" : "连接测试结果", out))
        } else if let first = out.first {
            messageText = first.message
        }
        await loadPage()
    }

    func deleteRows(_ rows: [ProxyManagedRow]) async {
        guard online else { return }
        busyLabel = "delete"
        defer { busyLabel = "" }
        do {
            if rows.count == 1 {
                try await RustBridge.shared.apiVoid("DELETE", "/proxy-management/\(rows[0].id)")
                selectedIds.remove(rows[0].id)
                dismissCurrentModal()
                messageText = "已删除"
            } else {
                let res: DeleteProxyBatchReply = try await RustBridge.shared.api(
                    "POST",
                    "/proxy-management/batch-delete",
                    body: .object(["ids": .array(rows.map { .int(Int64($0.id)) })])
                )
                let skipped = res.skipped
                selectedIds = Set(skipped.map(\.id))
                let outcomeRows: [(name: String, ok: Bool, message: String)] = rows.map { p in
                    let ok = res.deleted_ids.contains(p.id)
                    let msg = ok ? "已删除" : (skipped.first(where: { $0.id == p.id })?.reason ?? "删除未确认")
                    return (p.name, ok, msg)
                }
                presentModal(.results("批量删除结果", outcomeRows))
            }
            await loadPage()
            store?.refresh()
        } catch {
            errorText = error.localizedDescription
        }
    }

    func showCredentials(_ row: ProxyManagedRow) async {
        do {
            let cred: ProxyCredentialsDTO = try await RustBridge.shared.api("GET", "/proxy-management/\(row.id)/credentials")
            presentModal(.credentials(row, cred))
        } catch {
            errorText = error.localizedDescription
        }
    }

    func showAccounts(_ row: ProxyManagedRow) async {
        do {
            let list: [AccountIdentity] = try await RustBridge.shared.api("GET", "/proxy-management/\(row.id)/accounts")
            presentModal(.accounts(row, list))
        } catch {
            errorText = error.localizedDescription
        }
    }

    func importJSON() async {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.json]
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let text = try String(contentsOf: url, encoding: .utf8)
            let val = try ProxyDataParser.parseImport(text)
            let r: ProxyImportReply = try await RustBridge.shared.api(
                "POST",
                "/proxy-management/data",
                body: .object(["data": val])
            )
            messageText = "新增 \(r.proxy_created) 个，复用 \(r.proxy_reused) 个，失败 \(r.proxy_failed) 个"
            if !r.errors.isEmpty {
                presentModal(.results(
                    "导入结果",
                    r.errors.map { ($0.name.isEmpty ? "代理" : $0.name, false, $0.message) }
                ))
            }
            await loadPage()
        } catch {
            errorText = error.localizedDescription
        }
    }

    func exportJSON() async {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        panel.nameFieldStringValue = "sub2api-proxies.json"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let raw: JSONValue = try await RustBridge.shared.api("GET", "/proxy-management/data?\(buildQueryString(exporting: true))")
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            let data = try encoder.encode(raw)
            try data.write(to: url, options: .atomic)
            messageText = "已导出代理 JSON"
        } catch {
            errorText = error.localizedDescription
        }
    }
}
