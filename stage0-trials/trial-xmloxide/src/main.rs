use xmloxide::xpath::{parser, XPathContext, XPathValue};
use xmloxide::Document;

const XML: &str = r#"<html><body><main>
<section id="products"><div class="product-list">
<article class="product" data-id="1"><h3>Product 1</h3><span class="price">$10.99</span></article>
<article class="product" data-id="2"><h3>Product 2</h3><span class="price">$20.99</span></article>
<article class="product" data-id="3"><h3>Product 3</h3><span class="price">$15.99</span></article>
</div></section>
<section id="reviews"><div class="review-list">
<div class="review" data-rating="5"><p>Great</p></div>
<div class="review" data-rating="4"><p>Good</p></div>
<div class="review" data-rating="2"><p>Bad</p></div>
</div></section>
<table><tr><td>Cell 1</td><td>Cell 2</td></tr></table>
</main></body></html>"#;

fn main() {
    let doc = Document::parse_str(XML).expect("xml parse");
    let root = doc.root_element().unwrap();
    // For // queries context should be the document node; root element works
    // for descendant searches anchored at root in this implementation.
    let cases: Vec<(&str, &str, usize)> = vec![
        ("E1", "//section[@id=\"reviews\"]//div[contains(@class, \"review\") and @data-rating >= 4]", 2),
        ("E2", "//article[contains(@class, \"product\")][number(translate(substring-after(.//span[@class=\"price\"], \"$\") , \",\", \"\")) > 15]", 2),
        ("E4", "//td[count(ancestor::*) = 5]", 2),
    ];
    for (label, expr, expect) in &cases {
        match xmloxide::xpath::evaluate(&doc, root, expr) {
            Ok(XPathValue::NodeSet(ns)) => {
                println!("{} => nodeset {} [expect {}] {}", label, ns.len(), expect, if ns.len() == *expect { "PASS" } else { "FAIL" });
            }
            Ok(v) => println!("{} => non-nodeset {:?} [expect nodeset {}] FAIL", label, v, expect),
            Err(e) => println!("{} => ERR {:?} [expect nodeset {}] FAIL", label, e, expect),
        }
    }
    // E3 variable
    let parsed = parser::parse("//td[text()=$cell_text]").expect("parse E3");
    let mut ctx = XPathContext::new(&doc, root);
    ctx.set_variable("cell_text", XPathValue::String("Cell 1".to_string()));
    match ctx.evaluate(&parsed) {
        Ok(XPathValue::NodeSet(ns)) => println!("E3 => nodeset {} [expect 1] {}", ns.len(), if ns.len() == 1 { "PASS" } else { "FAIL" }),
        Ok(v) => println!("E3 => non-nodeset {:?} FAIL", v),
        Err(e) => println!("E3 => ERR {:?} FAIL", e),
    }
    // E5 invalid
    match xmloxide::xpath::evaluate(&doc, root, "4 ayo") {
        Err(e) => println!("E5 => ERR {:?} [expect error] PASS", e),
        Ok(v) => println!("E5 => Ok {:?} [expect error] FAIL", v),
    }
    println!("TRIAL-XMLoxide DONE");
}
