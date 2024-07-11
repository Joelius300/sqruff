use ahash::AHashMap;
use dyn_ord::DynEq;
use itertools::Itertools;

use crate::core::config::Value;
use crate::core::parser::segments::base::{
    ErasedSegment, NewlineSegment, NewlineSegmentNewArgs, WhitespaceSegment,
    WhitespaceSegmentNewArgs,
};
use crate::core::parser::segments::meta::{Indent, MetaSegmentKind};
use crate::core::rules::base::{CloneRule, ErasedRule, LintFix, LintResult, Rule, RuleGroups};
use crate::core::rules::context::RuleContext;
use crate::core::rules::crawlers::{Crawler, SegmentSeekerCrawler};
use crate::utils::functional::context::FunctionalContext;
use crate::utils::functional::segments::Segments;
use crate::utils::reflow::reindent::construct_single_indent;

#[derive(Clone, Debug, Default)]
pub struct RuleST04;

impl Rule for RuleST04 {
    fn load_from_config(&self, _config: &AHashMap<String, Value>) -> Result<ErasedRule, String> {
        Ok(RuleST04.erased())
    }

    fn name(&self) -> &'static str {
        "structure.nested_case"
    }

    fn is_fix_compatible(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "Nested ``CASE`` statement in ``ELSE`` clause could be flattened."
    }

    fn long_description(&self) -> &'static str {
        r"
## Anti-pattern

In this example, the outer `CASE`'s `ELSE` is an unnecessary, nested `CASE`.

```sql
SELECT
  CASE
    WHEN species = 'Cat' THEN 'Meow'
    ELSE
    CASE
       WHEN species = 'Dog' THEN 'Woof'
    END
  END as sound
FROM mytable
```

## Best practice

Move the body of the inner `CASE` to the end of the outer one.

```sql
SELECT
  CASE
    WHEN species = 'Cat' THEN 'Meow'
    WHEN species = 'Dog' THEN 'Woof'
  END AS sound
FROM mytable
```
"
    }

    fn groups(&self) -> &'static [RuleGroups] {
        &[RuleGroups::All, RuleGroups::Structure]
    }

    fn eval(&self, context: RuleContext) -> Vec<LintResult> {
        let segment = FunctionalContext::new(context.clone()).segment();
        let case1_children = segment.children(None);
        let case1_keywords =
            case1_children.find_first(Some(|it: &ErasedSegment| it.is_keyword("CASE")));
        let case1_first_case = case1_keywords.first().unwrap();
        let case1_when_list = case1_children.find_first(Some(|it: &ErasedSegment| {
            matches!(it.get_type(), "when_clause" | "else_clause")
        }));
        let case1_first_when = case1_when_list.first().unwrap();
        let when_clause_list = case1_children.find_last(Some(|it| it.is_type("when_clause")));
        let case1_last_when = when_clause_list.first();
        let case1_else_clause = case1_children.find_last(Some(|it| it.is_type("else_clause")));
        let case1_else_expressions =
            case1_else_clause.children(Some(|it| it.is_type("expression")));
        let expression_children = case1_else_expressions.children(None);
        let case2 = expression_children.select(None, None, None, None);
        let case2_children = case2.children(None);
        let case2_case_list =
            case2_children.find_first(Some(|it: &ErasedSegment| it.is_keyword("CASE")));
        let case2_first_case = case2_case_list.first();
        let case2_when_list = case2_children.find_first(Some(|it: &ErasedSegment| {
            matches!(it.get_type(), "when_clause" | "else_clause")
        }));
        let case2_first_when = case2_when_list.first();

        let Some(case1_last_when) = case1_last_when else { return Vec::new() };
        if case1_else_expressions.len() > 1 || expression_children.len() > 1 || case2.is_empty() {
            return Vec::new();
        }

        let x1 = segment
            .children(Some(|it| it.is_code()))
            .select(None, None, case1_first_case.into(), case1_first_when.into())
            .into_iter()
            .map(|it| it.get_raw_upper().unwrap());

        let x2 = case2
            .children(Some(|it| it.is_code()))
            .select(None, None, case2_first_case, case2_first_when)
            .into_iter()
            .map(|it| it.get_raw_upper().unwrap());

        if x1.ne(x2) {
            return Vec::new();
        }

        let case1_else_clause_seg = case1_else_clause.first().unwrap();

        let case1_to_delete =
            case1_children.select(None, None, case1_last_when.into(), case1_else_clause_seg.into());

        let comments = case1_to_delete.find_last(Some(|it: &ErasedSegment| it.is_comment()));
        let after_last_comment_index = comments
            .first()
            .and_then(|comment| case1_to_delete.iter().position(|it| it == comment))
            .map_or(0, |n| n + 1);

        let case1_comments_to_restore = case1_to_delete.select(
            None,
            None,
            None,
            case1_to_delete.base.get(after_last_comment_index),
        );
        let after_else_comment = case1_else_clause.children(None).select(
            Some(|it| {
                matches!(
                    it.get_type(),
                    "newline" | "inline_comment" | "block_comment" | "comment" | "whitespace"
                )
            }),
            None,
            None,
            case1_else_expressions.first(),
        );

        let mut fixes = case1_to_delete.into_iter().map(LintFix::delete).collect_vec();

        let tab_space_size =
            context.config.unwrap().raw["indentation"]["tab_space_size"].as_int().unwrap() as usize;
        let indent_unit =
            context.config.unwrap().raw["indentation"]["indent_unit"].as_string().unwrap();

        let when_indent_str =
            indentation(&case1_children, case1_last_when, tab_space_size, indent_unit);
        let end_indent_str =
            indentation(&case1_children, case1_first_case, tab_space_size, indent_unit);

        let nested_clauses = case2.children(Some(|it: &ErasedSegment| {
            matches!(
                it.get_type(),
                "when_clause"
                    | "else_clause"
                    | "newline"
                    | "inline_comment"
                    | "block_comment"
                    | "comment"
                    | "whitespace"
            )
        }));

        let mut segments = case1_comments_to_restore.base;
        segments.append(&mut rebuild_spacing(&when_indent_str, after_else_comment));
        segments.append(&mut rebuild_spacing(&when_indent_str, nested_clauses));

        fixes.push(LintFix::create_after(case1_last_when.clone(), segments, None));
        fixes.push(LintFix::delete(case1_else_clause_seg.clone()));
        fixes.append(&mut nested_end_trailing_comment(
            case1_children,
            case1_else_clause_seg,
            &end_indent_str,
        ));

        vec![LintResult::new(case2.first().cloned(), fixes, None, None, None)]
    }

    fn crawl_behaviour(&self) -> Crawler {
        SegmentSeekerCrawler::new(["case_expression"].into()).into()
    }
}

fn indentation(
    parent_segments: &Segments,
    segment: &ErasedSegment,
    tab_space_size: usize,
    indent_unit: &str,
) -> String {
    let leading_whitespace = parent_segments
        .select(None, None, None, segment.into())
        .reversed()
        .find_first(Some(|it: &ErasedSegment| it.is_type("whitespace")));
    let seg_indent = parent_segments
        .select(None, None, None, segment.into())
        .find_last(Some(|it| it.is_type("indent")));
    let mut indent_level = 1;
    if let Some(segment_indent) = seg_indent.last()
        && let Some(segment_indent) = segment_indent.as_any().downcast_ref::<Indent>()
    {
        indent_level = segment_indent.indent_val() as usize + 1;
    }

    let indent_str = if let Some(whitespace_seg) = leading_whitespace.first() {
        if !leading_whitespace.is_empty() && whitespace_seg.raw().len() > 1 {
            leading_whitespace.iter().map(|seg| seg.raw().to_string()).collect::<String>()
        } else {
            construct_single_indent(indent_unit, tab_space_size).repeat(indent_level)
        }
    } else {
        construct_single_indent(indent_unit, tab_space_size).repeat(indent_level)
    };
    indent_str
}

fn rebuild_spacing(indent_str: &str, nested_clauses: Segments) -> Vec<ErasedSegment> {
    let mut buff = Vec::new();

    let mut prior_newline = nested_clauses
        .find_last(Some(|it: &ErasedSegment| !it.is_whitespace()))
        .any(Some(|it: &ErasedSegment| it.is_comment()));
    let mut prior_whitespace = String::new();

    for seg in nested_clauses {
        if matches!(seg.get_type(), "when_clause" | "else_clause")
            || (prior_newline && seg.is_comment())
        {
            buff.push(NewlineSegment::create("\n", None, NewlineSegmentNewArgs {}));
            buff.push(WhitespaceSegment::create(indent_str, None, WhitespaceSegmentNewArgs {}));
            buff.push(seg.clone());
            prior_newline = false;
            prior_whitespace.clear();
        } else if seg.is_type("newline") {
            prior_newline = true;
            prior_whitespace.clear();
        } else if !prior_newline && seg.is_comment() {
            buff.push(WhitespaceSegment::create(
                &prior_whitespace,
                None,
                WhitespaceSegmentNewArgs {},
            ));
            buff.push(seg.clone());
            prior_newline = false;
            prior_whitespace.clear();
        } else if seg.is_whitespace() {
            prior_whitespace = seg.raw().to_string();
        }
    }

    buff
}

fn nested_end_trailing_comment(
    case1_children: Segments,
    case1_else_clause_seg: &ErasedSegment,
    end_indent_str: &str,
) -> Vec<LintFix> {
    // Prepend newline spacing to comments on the final nested `END` line.
    let trailing_end = case1_children.select(
        None,
        Some(|seg: &ErasedSegment| !seg.is_type("newline")),
        Some(case1_else_clause_seg),
        None,
    );

    let mut fixes = trailing_end
        .select(
            Some(|seg: &ErasedSegment| seg.is_whitespace()),
            Some(|seg: &ErasedSegment| !seg.is_comment()),
            None,
            None,
        )
        .into_iter()
        .map(LintFix::delete)
        .collect_vec();

    if let Some(first_comment) =
        trailing_end.find_first(Some(|seg: &ErasedSegment| seg.is_comment())).first()
    {
        let segments = vec![
            NewlineSegment::create("\n", None, NewlineSegmentNewArgs {}),
            WhitespaceSegment::create(end_indent_str, None, WhitespaceSegmentNewArgs {}),
        ];
        fixes.push(LintFix::create_before(first_comment.clone(), segments));
    }

    fixes
}