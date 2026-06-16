from __future__ import annotations

import shutil
import subprocess
import textwrap
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


class UsageQueryUITests(unittest.TestCase):
    def test_speed_template_exposes_quota_column_and_lazy_editor(self) -> None:
        template = (REPO_ROOT / "app" / "templates" / "speed.html").read_text(encoding="utf-8")
        partial = (REPO_ROOT / "app" / "templates" / "usage_query_editor.html").read_text(encoding="utf-8")

        self.assertIn('data-column="quota"', template)
        self.assertIn('<th data-col="quota">额度</th>', template)
        self.assertNotIn('data-column="state"', template)
        self.assertNotIn('data-column="priority"', template)
        self.assertNotIn('<th data-col="state">状态</th>', template)
        self.assertNotIn('<th data-col="priority">优先级</th>', template)
        self.assertIn('colspan="8"', template)
        self.assertIn('data-usage-query-editor-url="{{ base_path }}/usage-query/accounts/{{ row.id }}/editor', template)
        self.assertIn('data-usage-query-editor-target', template)
        self.assertNotIn('class="usage-query-form"', template)
        self.assertNotIn('name="template_type"', template)
        self.assertNotIn('name="access_token"', template)
        self.assertIn('action="{{ base_path }}/usage-query/accounts/{{ account_id }}"', partial)
        self.assertIn('name="template_type"', partial)
        self.assertIn('name="access_token"', partial)
        self.assertIn('name="user_id"', partial)
        self.assertIn('name="timeout_seconds"', partial)
        self.assertNotIn('Base URL / API Key：从账号本体实时读取', template)
        self.assertNotIn('Base URL / API Key：从账号本体实时读取', partial)
        self.assertNotIn('name="base_url"', template + partial)
        self.assertNotIn('name="api_key"', template + partial)
        self.assertNotIn("Sub2API API Key", template + partial)
        self.assertNotIn("cfg.api_key_saved", template + partial)
        self.assertNotIn('name="use_account_credentials"', template + partial)
        self.assertNotIn('fill-credentials', template + partial)
        self.assertIn('name="upstream_multiplier"', partial)
        self.assertNotIn('class="checkbox-label usage-query-toggle"', template + partial)
        self.assertNotIn('class="checkbox-label usage-query-guard-toggle"', template + partial)
        self.assertNotIn('name="enabled"', template + partial)
        self.assertNotIn('保存并查询', template + partial)
        self.assertNotIn('formaction="{{ base_path }}/usage-query/accounts/{{ row.id }}/query"', template + partial)
        self.assertIn('action="{{ base_path }}/usage-query/settings"', template)
        self.assertIn('name="usage_query_enabled"', template)
        self.assertIn('name="guard_disable_on_zero"', template)
        self.assertIn('name="auto_query_interval_seconds"', template)
        self.assertNotIn('name="sub2api_admin_token"', template)
        self.assertNotIn('name="auto_query_interval_minutes"', template)
        self.assertIn('自动间隔（秒）', template)
        self.assertIn('id="usage-query-{{ row.id }}"', template)
        self.assertIn('name="return_to" value="{{ return_to }}"', partial)

    def test_usage_query_hash_script_opens_and_lazy_loads_target_editor(self) -> None:
        base_template = (REPO_ROOT / "app" / "templates" / "base.html").read_text(encoding="utf-8")
        script = (REPO_ROOT / "app" / "static" / "usage-query.js").read_text(encoding="utf-8")

        self.assertIn("usage-query.js", base_template)
        self.assertIn("usage-query.js?v=20260617-light", base_template)
        self.assertIn("{% if active == 'speed' %}", base_template)
        self.assertIn("location.hash", script)
        self.assertIn("usage-query-", script)
        self.assertIn("details.open = true", script)
        self.assertIn("data-usage-query-editor-url", script)
        self.assertIn("data-usage-query-editor-loaded", script)
        self.assertIn("fetch(", script)
        self.assertIn("scrollIntoView", script)

    def test_navigation_busy_script_marks_internal_get_links_only(self) -> None:
        base_template = (REPO_ROOT / "app" / "templates" / "base.html").read_text(encoding="utf-8")
        script = (REPO_ROOT / "app" / "static" / "navigation.js").read_text(encoding="utf-8")

        self.assertIn("navigation.js", base_template)
        self.assertIn("navigation.js?v=20260617-light", base_template)
        self.assertIn("style.css?v=20260617-light", base_template)
        self.assertIn("guard-sections.js?v=20260617-light", base_template)
        self.assertIn("{% if active == 'guard' %}", base_template)
        self.assertIn("{% if active in ['speed', 'guard'] %}", base_template)
        self.assertNotIn("账号稳定性", base_template)
        self.assertNotIn("错误链路", base_template)
        self.assertNotIn("定时恢复", base_template)
        self.assertNotIn("schedule-options.js", base_template)
        self.assertIn("navigation-pending", script)
        self.assertIn("X-Sub2Ops-Prefetch", script)
        self.assertIn("requestIdleCallback", script)
        self.assertIn("event.defaultPrevented", script)
        self.assertIn('getAttribute("target") === "_blank"', script)
        self.assertIn("isSameDocumentNavigation", script)

    def test_guard_template_lazy_loads_heavy_sections(self) -> None:
        template = (REPO_ROOT / "app" / "templates" / "guard.html").read_text(encoding="utf-8")
        script = (REPO_ROOT / "app" / "static" / "guard-sections.js").read_text(encoding="utf-8")

        self.assertIn("data-guard-section-url", template)
        self.assertIn("/guard/sections/queue", template)
        self.assertIn("/guard/sections/suggestions", template)
        self.assertIn("/guard/sections/routing", template)
        self.assertIn("/guard/sections/audit", template)
        self.assertIn("data-guard-section-target", template)
        self.assertNotIn("guard-queue-card", template)
        self.assertNotIn("guard-routing-table", template)
        self.assertNotIn("guard-audit-table", template)
        self.assertNotIn("IntersectionObserver", script)
        self.assertIn("sectionCache", script)
        self.assertIn("data-guard-section-loaded", script)
        self.assertIn("fetch(", script)

    def test_speed_and_guard_templates_remove_explanatory_copy(self) -> None:
        speed = (REPO_ROOT / "app" / "templates" / "speed.html").read_text(encoding="utf-8")
        guard = (REPO_ROOT / "app" / "templates" / "guard.html").read_text(encoding="utf-8")
        telegram = (REPO_ROOT / "app" / "templates" / "telegram.html").read_text(encoding="utf-8")

        self.assertNotIn("按账号展示窗口内", speed)
        self.assertNotIn("速度统计和消耗", speed)
        self.assertNotIn("全局启用后会刷新", speed)
        self.assertNotIn("展开后加载", speed)
        self.assertNotIn("自动 Guard 读取全部账号", guard)
        self.assertNotIn("判定边界", guard)
        self.assertNotIn("这里控制 Guard", guard)
        self.assertNotIn("按需加载", guard)
        self.assertNotIn("错误链路", telegram)
        self.assertNotIn("定时测试自动恢复", telegram)

    def test_navigation_busy_script_behavior(self) -> None:
        node = shutil.which("node")
        if not node:
            self.skipTest("node is not available")
        script_path = REPO_ROOT / "app" / "static" / "navigation.js"
        test_script = textwrap.dedent(
            f"""
            const fs = require('fs');
            const vm = require('vm');
            const script = fs.readFileSync({str(script_path)!r}, 'utf8');

            function runClick(href, attrs = {{}}, eventAttrs = {{}}) {{
              const bodyClasses = new Set();
              const linkClasses = new Set();
              const link = {{
                href,
                hasAttribute: (name) => Boolean(attrs[name]),
                getAttribute: (name) => attrs[name] || null,
                classList: {{ add: (name) => linkClasses.add(name) }},
                setAttribute: (name, value) => {{ attrs[name] = value; }},
              }};
              let listener = null;
              const document = {{
                body: {{ classList: {{ add: (name) => bodyClasses.add(name) }} }},
                addEventListener: (type, callback) => {{ if (type === 'click') listener = callback; }},
              }};
              const window = {{ location: {{ href: 'https://ops.example.com/speed?group=a#top' }}, setTimeout: () => {{}} }};
              vm.runInNewContext(script, {{ document, window, URL, fetch: () => Promise.resolve() }});
              listener(Object.assign({{
                defaultPrevented: false,
                button: 0,
                metaKey: false,
                ctrlKey: false,
                shiftKey: false,
                altKey: false,
                target: {{ closest: () => link }},
              }}, eventAttrs));
              return {{
                bodyPending: bodyClasses.has('navigation-pending'),
                linkPending: linkClasses.has('pending'),
                ariaBusy: attrs['aria-busy'] || null,
              }};
            }}

            const cases = {{
              internal: runClick('https://ops.example.com/guard'),
              hashOnly: runClick('https://ops.example.com/speed?group=a#usage-query-9'),
              sameUrl: runClick('https://ops.example.com/speed?group=a#top'),
              external: runClick('https://external.example.com/speed'),
              blank: runClick('https://ops.example.com/guard', {{ target: '_blank' }}),
              download: runClick('https://ops.example.com/export', {{ download: '1' }}),
              modified: runClick('https://ops.example.com/guard', {{}}, {{ metaKey: true }}),
            }};
            const assert = require('assert');
            assert.deepStrictEqual(cases.internal, {{ bodyPending: true, linkPending: true, ariaBusy: 'true' }});
            for (const [name, result] of Object.entries(cases)) {{
              if (name === 'internal') continue;
              assert.deepStrictEqual(result, {{ bodyPending: false, linkPending: false, ariaBusy: null }}, name);
            }}
            """
        )
        subprocess.run([node, "-e", test_script], check=True)

    def test_navigation_prefetch_script_behavior(self) -> None:
        node = shutil.which("node")
        if not node:
            self.skipTest("node is not available")
        script_path = REPO_ROOT / "app" / "static" / "navigation.js"
        test_script = textwrap.dedent(
            f"""
            const fs = require('fs');
            const vm = require('vm');
            const script = fs.readFileSync({str(script_path)!r}, 'utf8');

            const listeners = {{}};
            const fetches = [];
            const link = {{
              href: 'https://ops.example.com/guard',
              hasAttribute: () => false,
              getAttribute: () => null,
              classList: {{ add: () => {{}} }},
              setAttribute: () => {{}},
            }};
            const document = {{
              body: {{ classList: {{ add: () => {{}} }} }},
              addEventListener: (type, callback) => {{ listeners[type] = callback; }},
            }};
            const window = {{
              location: {{ href: 'https://ops.example.com/speed' }},
              requestIdleCallback: (callback) => callback(),
              setTimeout: (callback) => callback(),
            }};
            vm.runInNewContext(script, {{
              document,
              window,
              URL,
              fetch: (url, options) => {{ fetches.push([url, options]); return Promise.resolve(); }},
            }});
            const event = {{
              metaKey: false,
              ctrlKey: false,
              shiftKey: false,
              altKey: false,
              target: {{ closest: () => link }},
            }};
            listeners.mouseover(event);
            listeners.mouseover(event);
            const assert = require('assert');
            assert.strictEqual(fetches.length, 1);
            assert.strictEqual(fetches[0][0], 'https://ops.example.com/guard');
            assert.strictEqual(fetches[0][1].credentials, 'same-origin');
            assert.strictEqual(fetches[0][1].headers['X-Sub2Ops-Prefetch'], '1');
            """
        )
        subprocess.run([node, "-e", test_script], check=True)

    def test_usage_query_styles_are_scoped_to_speed_quota_ui(self) -> None:
        style = (REPO_ROOT / "app" / "static" / "style.css").read_text(encoding="utf-8")

        self.assertIn(".quota-cell", style)
        self.assertIn(".usage-query-config", style)
        self.assertIn(".usage-query-editor-placeholder", style)
        self.assertIn("body.navigation-pending", style)
        self.assertNotIn("color-scheme: dark", style)
        self.assertNotIn("--bg: #010102", style)
        self.assertIn("--bg: #f6faf9", style)
        self.assertNotIn(".usage-query-credential-note", style)
        self.assertIn(".usage-query-template-select", style)
        self.assertIn("grid-template-columns: repeat(3, minmax(180px, 1fr));", style)
        self.assertIn("min-height: 108px;", style)
        self.assertNotIn("padding: 0 14px 14px 254px;", style)


if __name__ == "__main__":
    unittest.main()
