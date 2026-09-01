LEGACY_RECOVERY_PLAN_CLEANUP_SQL = """
DELETE FROM scheduled_test_plans
WHERE cron_expression = '* * * * *'
  AND auto_recover = true
RETURNING id, account_id
"""
