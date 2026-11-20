use super::*;
use crate::usage_records::Filter;

pub(super) struct QueryParams(BTreeMap<String, String>);
impl QueryParams {
    pub fn parse(request: &Request, route: Route<&str, i64>) -> Result<Self> {
        let pairs = Query::<Vec<(String, String)>>::try_from_uri(request.uri())
            .map_err(|_| Error::invalid())?
            .0;
        let count = pairs.len();
        let map = pairs.into_iter().collect::<BTreeMap<_, _>>();
        if matches!(
            route,
            Route::Priorities
                | Route::PriorityOrders
                | Route::PriorityOrder(_)
                | Route::CancelOrder(_)
        ) {
            if count != map.len()
                || request.uri().query().is_some_and(str::is_empty)
                || map.iter().any(|(name, value)| match name.as_str() {
                    "schema" => value != "2",
                    "details" => !matches!(route, Route::PriorityOrder(_)) || value != "0",
                    _ => true,
                })
            {
                return Err(Error::invalid());
            }
            if !map.contains_key("schema") {
                return Err(crate::priority_orders::upgrade_required());
            }
        }
        Ok(Self(map))
    }
    pub fn map(&self) -> &BTreeMap<String, String> {
        &self.0
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
    pub fn empty(&self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::invalid())
        }
    }
    pub fn number(&self, name: &str, min: i64, max: i64) -> Result<Option<i64>> {
        self.get(name)
            .map(|v| validation::coercible_integer(&json!(v), min, max))
            .transpose()
    }
    pub fn flag(&self, name: &str) -> Result<bool> {
        self.get(name)
            .map(|v| validation::coercible_boolean(&json!(v)))
            .transpose()
            .map(Option::unwrap_or_default)
    }
    pub fn limit(&self) -> Result<usize> {
        Ok(self.number("limit", 1, 100)?.unwrap_or(50) as usize)
    }
    pub fn category(&self) -> Result<Option<&str>> {
        match self.get("category") {
            None => Ok(None),
            Some(v @ ("degradation" | "other")) => Ok(Some(v)),
            _ => Err(Error::new(422, "错误分类无效")),
        }
    }
    pub fn usage_filter(&self) -> Result<Filter> {
        Ok(Filter {
            from_at: self.0.get("from_at").cloned(),
            to_at: self.0.get("to_at").cloned(),
            start_date: self.0.get("start_date").cloned(),
            end_date: self.0.get("end_date").cloned(),
            include_summary: self.flag("include_summary")?,
            account_id: self.number("account_id", i64::MIN, i64::MAX)?,
            platform: self.0.get("platform").filter(|v| !v.is_empty()).cloned(),
            api_key_id: self.number("api_key_id", i64::MIN, i64::MAX)?,
            user_id: self.number("user_id", i64::MIN, i64::MAX)?,
            model: self.0.get("model").cloned(),
            request_type: self.0.get("request_type").cloned(),
            mismatch_only: self.flag("mismatch_only")?,
            cursor: self.0.get("cursor").cloned(),
            after_id: self.number("after_id", i64::MIN, i64::MAX)?,
            limit: self.number("limit", 1, 100)?.map(|v| v as usize),
        })
    }
}
