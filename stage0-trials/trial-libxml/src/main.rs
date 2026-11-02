use libxml::parser::Parser;
use libxml::xpath::Context;

fn main() {
    let html = r#"<html><body><div id="list">
      <div class="product"><span class="price">$10.99</span></div>
      <div class="product"><span class="price">$20.99</span></div>
      <table><tr><td>Cell 1</td><td>Cell 2</td></tr></table>
    </div></body></html>"#;
    let parser = Parser::default_html();
    let doc = parser.parse_string(html).expect("parse html");
    let mut ctx = Context::new(&doc).expect("xpath ctx");
    let prices = ctx.findnodes("//span[@class=\"price\"]", None).expect("xpath");
    println!("prices found: {}", prices.len());
    for n in &prices {
        println!("price text: {}", n.get_content());
    }
    let root = doc.get_root_element().expect("root");
    println!("root: {}", root.get_name());
    let cells = ctx.findvalues("//td[text()=\"Cell 1\"]", None).expect("xpath2");
    println!("cells matching text()=Cell 1: {:?}", cells);
    assert_eq!(prices.len(), 2);
    println!("TRIAL-LIBXML OK");
}
