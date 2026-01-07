//!
//! Example program that displays all of the nodes from a public MEGA link
//! in a textual tree format.
//!

use text_trees::{FormatCharacters, StringTreeNode, TreeFormatting};

async fn construct_tree_node(nodes: &mega::Nodes, node: &mega::DecryptedNode, client: &mega::Client) -> StringTreeNode {
    let decryption_pack = client.decryption_pack(nodes).await.unwrap();
    let (mut folders, mut files): (Vec<_>, Vec<_>) = node
        .children()
        .iter()
        .filter_map(|hash| nodes.get_decrypted_node_by_handle(hash, &decryption_pack).unwrap())
        .partition(|node| node.r#type().is_folder());

    folders.sort_unstable_by_key(|node| node.name());
    files.sort_unstable_by_key(|node| node.name());

    let children = std::iter::empty()
        .chain(folders)
        .chain(files)
        .map(async |node| construct_tree_node(nodes, node, client).await);

    StringTreeNode::with_child_nodes(node.name().to_string(), children)
}

async fn run(mega: &mut mega::Client, public_url: &str) -> mega::Result<()> {
    let mut stdout = std::io::stdout().lock();

    let nodes = mega.fetch_public_nodes(public_url).await?;
    let formatting = TreeFormatting::dir_tree(FormatCharacters::box_chars());

    let decryption_pack = mega.decryption_pack(&nodes).await?;

    println!();
    for root in nodes.decrypted_roots(&decryption_pack) {
        let tree = construct_tree_node(&nodes, root, mega).await;
        tree.write_with_format(&mut stdout, &formatting).unwrap();
        println!();
    }

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let public_url = match args.as_slice() {
        [public_url] => public_url.as_str(),
        _ => {
            panic!("expected 1 command-line argument: {{public_url}}");
        }
    };

    let http_client = reqwest::Client::new();
    let mut mega = mega::Client::builder().build(http_client).unwrap();

    run(&mut mega, public_url).await.unwrap();
}
