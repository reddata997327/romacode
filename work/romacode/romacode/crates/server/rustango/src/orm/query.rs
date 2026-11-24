//! Lazy query sets — `django.db.models.QuerySet` with chaining:
//! `filter`, `exclude`, `order_by`, `count`, `exists`, `first`, `get`,
//! `take` (Django slicing), `delete`, `update` and `latest`.

use std::marker::PhantomData;

use super::backend::Backend;
use super::model::{Model, ModelExt};
use super::{Criteria, OrmResult, Row, Value};

/// A chainable, lazy query. `filter()`/`exclude()` return new query sets;
/// terminal methods hit the backend.
pub struct QuerySet<'a, T> {
    backend: &'a dyn Backend,
    crit: Criteria,
    order: Vec<(String, bool)>,
    limit: Option<usize>,
    offset: Option<usize>,
    _pd: PhantomData<T>,
}

impl<'a, T: Model> QuerySet<'a, T> {
    pub fn new(backend: &'a dyn Backend) -> Self {
        Self {
            backend,
            crit: Criteria::any(),
            order: Vec::new(),
            limit: None,
            offset: None,
            _pd: PhantomData,
        }
    }

    /// AND a criteria into the filter — `.filter(Q(...))`.
    pub fn filter(mut self, crit: Criteria) -> Self {
        self.crit = match (self.crit.is_any(), crit.is_any()) {
            (true, _) => crit,
            (_, true) => self.crit,
            _ => Criteria::And(vec![self.crit, crit]),
        };
        self
    }

    /// `filter(field__op=value)` shorthand.
    pub fn filter_field(
        self,
        field: impl Into<String>,
        op: super::Op,
        value: impl Into<Value>,
    ) -> Self {
        self.filter(Criteria::cond(field, op, value))
    }

    /// NOT the criteria — `.exclude(Q(...))`.
    pub fn exclude(self, crit: Criteria) -> Self {
        self.filter(Criteria::Not(Box::new(crit)))
    }

    /// `order_by("name")` / `order_by("-pub_date")`.
    pub fn order_by(mut self, field: &str) -> Self {
        let desc = field.starts_with('-');
        let name = field.trim_start_matches('-').to_string();
        self.order.push((name, desc));
        self
    }

    /// Django's `qs[:n]` slice.
    pub fn take(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Django's `qs[a:]` slice start.
    pub fn skip(mut self, n: usize) -> Self {
        self.offset = Some(n);
        self
    }

    /// Evaluate to rows.
    pub fn rows(&self) -> OrmResult<Vec<Row>> {
        self.backend.select(
            T::table(),
            &self.crit,
            &self.order,
            self.limit,
            self.offset,
        )
    }

    /// Evaluate to model instances — iterating the QuerySet.
    pub fn all(&self) -> OrmResult<Vec<T>> {
        Ok(self.rows()?.iter().map(|r| T::from_row(r)).collect())
    }

    /// `qs.count()`.
    pub fn count(&self) -> OrmResult<usize> {
        self.backend.count(T::table(), &self.crit)
    }

    /// `qs.exists()`.
    pub fn exists(&self) -> OrmResult<bool> {
        Ok(self.count()? > 0)
    }

    /// `qs.first()` — None when empty.
    pub fn first(&self) -> OrmResult<Option<T>> {
        let qs = Self {
            backend: self.backend,
            crit: self.crit.clone(),
            order: self.order.clone(),
            limit: Some(1),
            offset: self.offset,
            _pd: PhantomData,
        };
        Ok(qs.rows()?.into_iter().next().map(|r| T::from_row(&r)))
    }

    /// `qs.latest("field")` — first row under reversed order.
    pub fn latest(self, field: &str) -> OrmResult<T> {
        let desc = !field.starts_with('-');
        let name = field.trim_start_matches('-').to_string();
        let qs = self.order_by(&if desc { format!("-{name}") } else { name }).take(1);
        qs.first()?
            .ok_or_else(|| crate::errors::OrmError::DoesNotExist(format!("{} latest({})", T::table(), field)))
    }

    /// `qs.get(pk=..)` / `qs.get(Q(..))` — exactly one row or an error.
    pub fn get(self, crit: Criteria) -> OrmResult<T> {
        let qs = self.filter(crit).take(2);
        let rows = qs.rows()?;
        match rows.len() {
            0 => Err(crate::errors::OrmError::DoesNotExist(format!(
                "{} matching query does not exist",
                T::table()
            ))),
            1 => Ok(T::from_row(&rows[0])),
            _ => Err(crate::errors::OrmError::MultipleObjectsReturned(format!(
                "get() returned more than one {}",
                T::table()
            ))),
        }
    }

    /// Bulk UPDATE — `qs.update(values)` where values are (column_index,
    /// new_value). Use [`QuerySet::column_index`] to resolve indexes.
    pub fn update(&self, updates: &[(usize, Value)]) -> OrmResult<usize> {
        self.backend.update(T::table(), &self.crit, updates)
    }

    /// Bulk DELETE — returns number of rows removed.
    pub fn delete(&self) -> OrmResult<usize> {
        // FK traversal criteria may reference other tables; delete row by row
        // so model signals fire (Django also fires signals per-instance here).
        let rows = self.rows()?;
        let mut n = 0;
        for row in rows {
            let obj = T::from_row(&row);
            n += obj.delete(self.backend)?;
        }
        Ok(n)
    }
}

impl<'a, T: Model> Clone for QuerySet<'a, T> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend,
            crit: self.crit.clone(),
            order: self.order.clone(),
            limit: self.limit,
            offset: self.offset,
            _pd: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orm::InMemoryBackend;
    use std::sync::Arc;

    #[derive(Clone, Debug, crate::Model)]
#[model(table = "test_book")]
    pub struct Book {
        #[field(auto)]
        pub id: i64,
        #[field(char, max = 100)]
        pub title: String,
        #[field(int, default = 0)]
        pub author_id: i64,
        #[field(int, default = 0)]
        pub pages: i64,
        #[field(bool, default = false)]
        pub published: bool,
    }

    fn backend() -> Arc<InMemoryBackend> {
        let b = Arc::new(InMemoryBackend::new());
        b.create_table(Book::table(), Book::columns().to_vec()).unwrap();
        for (t, a, p, pubd) in [
            ("Rust Book", 1, 300, true),
            ("Django Book", 2, 450, true),
            ("Go Book", 1, 250, false),
            ("Zig Book", 3, 180, true),
        ] {
            let b0 = Book { id: 0, title: t.into(), author_id: a, pages: p, published: pubd };
            b0.save(&*b).unwrap();
        }
        b
    }

    #[test]
    fn all_and_count() {
        let b = backend();
        assert_eq!(Book::objects(&*b).all().unwrap().len(), 4);
        assert_eq!(Book::objects(&*b).count().unwrap(), 4);
        assert!(Book::objects(&*b).exists().unwrap());
    }

    #[test]
    fn filter_chain() {
        let b = backend();
        let qs = Book::objects(&*b)
            .filter(Criteria::eq("published", Value::Bool(true)))
            .filter(Criteria::gt("pages", Value::Int(200)));
        let books = qs.all().unwrap();
        assert_eq!(books.len(), 2); // rust(300), django(450)
        assert_eq!(qs.count().unwrap(), 2);
    }

    #[test]
    fn exclude() {
        let b = backend();
        let qs = Book::objects(&*b)
            .exclude(Criteria::eq("author_id", Value::Int(1)));
        assert_eq!(qs.count().unwrap(), 2); // django, zig
    }

    #[test]
    fn order_and_slice() {
        let b = backend();
        let books = Book::objects(&*b)
            .order_by("-pages")
            .all()
            .unwrap();
        assert_eq!(books[0].title, "Django Book");

        let top2 = Book::objects(&*b).order_by("-pages").take(2).all().unwrap();
        assert_eq!(top2.len(), 2);
        assert_eq!(top2[0].pages, 450);
        assert_eq!(top2[1].pages, 300);

        let skipped = Book::objects(&*b).order_by("pages").skip(1).take(2).all().unwrap();
        assert_eq!(skipped[0].pages, 250);
    }

    #[test]
    fn get_semantics() {
        let b = backend();
        let one = Book::objects(&*b)
            .get(Criteria::eq("title", Value::Text("Go Book".into())))
            .unwrap();
        assert_eq!(one.author_id, 1);

        assert!(Book::objects(&*b)
            .get(Criteria::eq("title", Value::Text("Nope".into())))
            .is_err());

        // two matches -> MultipleObjectsReturned
        let err = Book::objects(&*b).get(Criteria::gt("pages", Value::Int(200)));
        assert_eq!(
            err.unwrap_err(),
            crate::errors::OrmError::MultipleObjectsReturned("get() returned more than one test_book".into())
        );
    }

    #[test]
    fn first_and_latest() {
        let b = backend();
        let f = Book::objects(&*b).order_by("title").first().unwrap().unwrap();
        assert_eq!(f.title, "Django Book");
        assert!(Book::objects(&*b)
            .filter(Criteria::eq("title", Value::Text("zzz".into())))
            .first()
            .unwrap()
            .is_none());

        let latest = Book::objects(&*b).latest("pages").unwrap();
        assert_eq!(latest.pages, 450);
    }

    #[test]
    fn bulk_update() {
        let b = backend();
        let idx = Book::column_index("published").unwrap();
        let n = Book::objects(&*b)
            .filter(Criteria::eq("author_id", Value::Int(1)))
            .update(&[(idx, Value::Bool(true))])
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            Book::objects(&*b)
                .filter(Criteria::eq("published", Value::Bool(true)))
                .count()
                .unwrap(),
            4
        );
    }

    #[test]
    fn bulk_delete() {
        let b = backend();
        let n = Book::objects(&*b)
            .filter(Criteria::eq("author_id", Value::Int(1)))
            .delete()
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(Book::objects(&*b).count().unwrap(), 2);
    }

    #[test]
    fn q_or_semantics() {
        let b = backend();
        let qs = Book::objects(&*b).filter(
            Criteria::eq("author_id", Value::Int(2)) | Criteria::eq("author_id", Value::Int(3)),
        );
        assert_eq!(qs.count().unwrap(), 2);
    }

    #[test]
    fn queryset_clone_and_reuse() {
        let b = backend();
        let base = Book::objects(&*b).filter(Criteria::eq("published", Value::Bool(true)));
        let a = base.clone().order_by("pages").all().unwrap();
        let c = base.clone().count().unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(c, 3);
        // original unchanged
        assert_eq!(base.count().unwrap(), 3);
    }
}
