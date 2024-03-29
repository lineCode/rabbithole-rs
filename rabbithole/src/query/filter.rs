use crate::model::error;

use crate::RbhResult;

use rsql_rs::ast::expr::Expr;

#[cfg(feature = "filter_rsql")]
use rsql_rs::ast::comparison;
#[cfg(feature = "filter_rsql")]
use rsql_rs::ast::comparison::Comparison;
#[cfg(feature = "filter_rsql")]
use rsql_rs::ast::constraint::Constraint;
#[cfg(feature = "filter_rsql")]
use rsql_rs::ast::Operator;
#[cfg(feature = "filter_rsql")]
use rsql_rs::parser::rsql::RsqlParser;
#[cfg(feature = "filter_rsql")]
use rsql_rs::parser::Parser;

use crate::entity::SingleEntity;
#[cfg(feature = "filter_rsql")]
use std::cmp::Ordering;
use std::collections::HashMap;

pub trait FilterData: Sized {
    fn new(params: &HashMap<String, String>) -> RbhResult<Option<Self>>;

    fn filter<E: SingleEntity>(&self, entities: Vec<E>) -> RbhResult<Vec<E>>;
}

/// Example:
/// `?include=authors&filter[book]=title==*Foo*&filter[author]=name!='Orson Scott Card'`
/// where key is self type or relationship name
#[derive(Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct RsqlFilterData(HashMap<String, Expr>);

impl FilterData for RsqlFilterData {
    #[cfg(not(feature = "filter_rsql"))]
    fn new(_params: &HashMap<String, String>) -> RbhResult<Option<Self>> {
        Err(error::Error::RsqlFilterNotImplemented(None))
    }

    #[cfg(feature = "filter_rsql")]
    fn new(params: &HashMap<String, String>) -> RbhResult<Option<Self>> {
        let mut res: HashMap<String, Expr> = Default::default();
        for (k, v) in params.into_iter() {
            if k.contains('.') {
                return Err(error::Error::RelationshipPathNotSupported(&k, None));
            }
            let expr = RsqlParser::parse_to_node(v)
                .map_err(|_| error::Error::UnmatchedFilterItem("Rsql", &k, &v, None))?;
            res.insert(k.clone(), expr);
        }
        Ok(if res.is_empty() { None } else { Some(RsqlFilterData(res)) })
    }

    #[cfg(not(feature = "filter_rsql"))]
    fn filter<E: SingleEntity>(&self, _entities: Vec<E>) -> RbhResult<Vec<E>> { unimplemented!() }

    #[cfg(feature = "filter_rsql")]
    fn filter<E: SingleEntity>(&self, mut entities: Vec<E>) -> RbhResult<Vec<E>> {
        for (ty_or_relat, expr) in &self.0 {
            entities = entities
                .into_iter()
                .filter_map(|r| {
                    match (&E::ty() == ty_or_relat, Self::filter_on_attributes(expr, &r)) {
                        (true, Ok(true)) => Some(Ok(r)),
                        (true, Ok(false)) => None,
                        (true, Err(err)) => Some(Err(err)),
                        (false, _) => {
                            Some(Err(error::Error::RsqlFilterOnRelatedNotImplemented(None)))
                        },
                    }
                })
                .collect::<RbhResult<Vec<E>>>()?;
        }
        Ok(entities)
    }
}

impl RsqlFilterData {
    #[cfg(feature = "filter_rsql")]
    pub fn filter_on_attributes<E: SingleEntity>(expr: &Expr, entity: &E) -> RbhResult<bool> {
        let ent: bool = match &expr {
            Expr::Item(Constraint { selector, comparison, arguments }) => {
                if let Ok(field) = entity.attributes().get_field(&selector) {
                    if comparison == &comparison::EQUAL as &Comparison && arguments.0.len() == 1 {
                        let arg: &str = arguments.0.first().unwrap();
                        field.eq_with_str(arg, &selector)?
                    } else if comparison == &comparison::NOT_EQUAL as &Comparison
                        && arguments.0.len() == 1
                    {
                        let arg: &str = arguments.0.first().unwrap();
                        field.eq_with_str(arg, &selector)? == false
                    } else if comparison == &comparison::GREATER_THAN as &Comparison
                        && arguments.0.len() == 1
                    {
                        let arg: &str = arguments.0.first().unwrap();
                        field.cmp_with_str(arg, &selector)? == Ordering::Greater
                    } else if comparison == &comparison::GREATER_THAN_OR_EQUAL as &Comparison
                        && arguments.0.len() == 1
                    {
                        let arg: &str = arguments.0.first().unwrap();
                        let res = field.cmp_with_str(arg, &selector)?;
                        res == Ordering::Greater || res == Ordering::Equal
                    } else if comparison == &comparison::LESS_THAN as &Comparison
                        && arguments.0.len() == 1
                    {
                        let arg: &str = arguments.0.first().unwrap();
                        let res = field.cmp_with_str(arg, &selector)?;
                        res == Ordering::Less
                    } else if comparison == &comparison::LESS_THAN_OR_EQUAL as &Comparison
                        && arguments.0.len() == 1
                    {
                        let arg: &str = arguments.0.first().unwrap();
                        let res = field.cmp_with_str(arg, &selector)?;
                        res == Ordering::Less || res == Ordering::Equal
                    } else if comparison == &comparison::IN as &Comparison {
                        arguments
                            .0
                            .iter()
                            .find(|s| field.eq_with_str(s, &selector).is_ok())
                            .is_some()
                    } else if comparison == &comparison::OUT as &Comparison {
                        arguments
                            .0
                            .iter()
                            .find(|s| field.eq_with_str(s, &selector).is_ok())
                            .is_none()
                    } else {
                        Err(error::Error::UnsupportedRsqlComparison(
                            &comparison.symbols,
                            arguments.0.len(),
                            None,
                        ))?
                    }
                } else {
                    Err(error::Error::FieldNotExist(&selector, None))?
                }
            },
            Expr::Node(op, left, right) => {
                let left = Self::filter_on_attributes(left, entity)?;
                match op {
                    Operator::And => left && Self::filter_on_attributes(right, entity)?,
                    Operator::Or => left || Self::filter_on_attributes(right, entity)?,
                }
            },
        };
        Ok(ent)
    }
}

#[derive(Debug)]
pub enum FilterQuery {
    Rsql(RsqlFilterData),
}

impl FilterQuery {
    pub fn new(ty: &str, params: &HashMap<String, String>) -> RbhResult<Option<FilterQuery>> {
        if ty == "Rsql" {
            RsqlFilterData::new(params).map(|op| op.map(FilterQuery::Rsql))
        } else {
            Err(error::Error::InvalidFilterType(ty, None))
        }
    }

    pub fn filter<E: SingleEntity>(&self, entities: Vec<E>) -> RbhResult<Vec<E>> {
        match &self {
            FilterQuery::Rsql(map) => RsqlFilterData::filter(map, entities),
        }
    }
}
