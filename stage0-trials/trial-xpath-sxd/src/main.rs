use sxd_document::parser;
use sxd_xpath::{Context, Factory, Value};

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

fn run(factory: &Factory, ctx: &Context, doc: &sxd_document::Package, expr: &str) -> String {
    let built = factory.build(expr);
    match built {
        Ok(Some(xp)) => {
            let root = doc.as_document().root();
            match xp.evaluate(ctx, root) {
                Ok(Value::Nodeset(ns)) => format!("nodeset {}", ns.size()),
                Ok(v) => format!("value {:?}", v),
                Err(e) => format!("EVAL-ERR {:?}", e),
            }
        }
        Ok(None) => "BUILD-NONE".to_string(),
        Err(e) => format!("BUILD-ERR {:?}", e),
    }
}

fn main() {
    let package = parser::parse(XML).expect("xml parse");
    let factory = Factory::new();
    let mut ctx = Context::new();
    ctx.set_variable("cell_text", "Cell 1");

    let cases: Vec<(&str, &str, &str)> = vec![
        ("E1 reviews rating>=4 (expect nodeset 2)",
         "//section[@id=\"reviews\"]//div[contains(@class, \"review\") and @data-rating >= 4]", "nodeset 2"),
        ("E2 price number(translate(substring-after))>15 (expect nodeset 2)",
         "//article[contains(@class, \"product\")][number(translate(substring-after(.//span[@class=\"price\"], \"$\") , \",\", \"\")) > 15]", "nodeset 2"),
        ("E3 variable //td[text()=$cell_text] (expect nodeset 1)",
         "//td[text()=$cell_text]", "nodeset 1"),
        ("E4 count(ancestor::*) =5 for td (expect nodeset 2)",
         "//td[count(ancestor::*) = 5]", "nodeset 2"),
        ("E5 invalid '4 ayo' (expect error)", "4 ayo", "ERR"),
    ];
    for (label, expr, expect) in &cases {
        let got = run(&factory, &ctx, &package, expr);
        let ok = got.contains(expect) || (*expect == "ERR" && got.contains("ERR"));
        println!("{} => {} [expect {}] {}", label, got, expect, if ok { "PASS" } else { "FAIL" });
    }
    println!("TRIAL-SXD DONE");
}
