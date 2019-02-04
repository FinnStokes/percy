//! The virtual_node module exposes the `VirtualNode` struct and methods that power our
//! virtual dom.

// TODO: A few of these dependencies (including js_sys) are used to power events.. yet events
// only work on wasm32 targest. So we should start sprinkling some
//
// #[cfg(target_arch = "wasm32")]
// #[cfg(not(target_arch = "wasm32"))]
//
// Around in order to get rid of dependencies that we don't need in non wasm32 targets

pub use std::cell::RefCell;
use std::collections::{HashSet,HashMap};
use std::fmt;
pub use std::rc::Rc;

pub mod virtual_node_test_utils;

use web_sys::{self, Text, Element, Node, EventTarget};

use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

use lazy_static::lazy_static;

use std::ops::Deref;
use std::sync::Mutex;


// Used to uniquely identify elements that contain closures so that the DomUpdater can
// look them up by their unique id.
// When the DomUpdater sees that the element no longer exists it will drop all of it's
// Rc'd Closures for those events.
lazy_static! {
    static ref ELEM_UNIQUE_ID: Mutex<u32> = Mutex::new(0);

    static ref SELF_CLOSING_TAGS: HashSet<&'static str> = [
        "area", "base", "br", "col", "hr", "img", "input", "link", "meta",
        "param", "command", "keygen", "source",
    ].iter().cloned().collect();
}

/// When building your views you'll typically use the `html!` macro to generate
/// `VirtualNode`'s.
///
/// `html! { <div> <span></span> </div> }` really generates a `VirtualNode` with
/// one child (span).
///
/// Later, on the client side, you'll use the `diff` and `patch` modules to
/// update the real DOM with your latest tree of virtual nodes (virtual dom).
///
/// Or on the server side you'll just call `.to_string()` on your root virtual node
/// in order to recursively render the node and all of its children.
///
/// TODO: Make all of these fields private and create accessor methods
#[derive(Debug, PartialEq)]
pub enum VirtualNode {
    /// An element node (node type `ELEMENT_NODE`).
    Element(VirtualNodeElement),
    /// A text node (node type `TEXT_NODE`).
    Text(VirtualNodeText),
}

#[derive(PartialEq)]
pub struct VirtualNodeElement {
    /// The HTML tag, such as "div"
    pub tag: String,
    /// HTML props such as id, class, style, etc
    pub props: HashMap<String, String>,
    /// Events that will get added to your real DOM element via `.addEventListener`
    pub events: Events,
    /// The children of this `VirtualNode`. So a <div> <em></em> </div> structure would
    /// have a parent div and one child, em.
    pub children: Option<Vec<VirtualNode>>,
}

#[derive(PartialEq)]
pub struct VirtualNodeText {
    pub text: String,
}

impl VirtualNode {
    /// Create a new virtual node with a given tag.
    ///
    /// These get patched into the DOM using `document.createElement`
    ///
    /// ```ignore
    /// use virtual_dom_rs::VirtualNode;
    ///
    /// let div = VirtualNode::new("div");
    /// ```
    pub fn new(tag: &str) -> VirtualNode {
        let props = HashMap::new();
        let custom_events = Events(HashMap::new());
        VirtualNode::Element(VirtualNodeElement {
            tag: tag.to_string(),
            props,
            events: custom_events,
            children: Some(vec![]),
        })
    }

    /// Create and return a `CreatedNode` instance (containing a DOM `Node`
    /// together with potentially related closures) for this virtual node.
    pub fn create_dom_node(&self) -> CreatedNode {
        match self {
            VirtualNode::Text(text_node) => CreatedNode::without_closures(text_node.create_text_node()),
            VirtualNode::Element(element_node) => element_node.create_element_node().into(),
        }
    }

    /// Create a text node.
    ///
    /// These get patched into the DOM using `document.createTextNode`
    ///
    /// ```ignore
    /// use virtual_dom_rs::VirtualNode;
    ///
    /// let div = VirtualNode::text("div");
    /// ```
    #[deprecated(note = "Use the .into() method on a string or string slice instead")] // TODO remove
    pub fn text(text: &str) -> VirtualNode {
        text.into()
    }

    /// Whether or not this `VirtualNode` is representing a `Text` node
    #[deprecated(note="Use enum matching instead")]  // TODO remove this method
    pub fn is_text_node(&self) -> bool {
        match self {
            VirtualNode::Text(_) => true,
            _ => false,
        }
    }
}

impl VirtualNodeElement {
    /// Whether or not this is a self closing tag such as <br> or <img />
    pub fn is_self_closing(&self) -> bool {
        SELF_CLOSING_TAGS.contains(self.tag.as_str())
    }

    /// Build a DOM element by recursively creating DOM nodes for this element and it's
    /// children, it's children's children, etc.
    pub fn create_element_node(&self) -> CreatedElement {
        let document = web_sys::window().unwrap().document().unwrap();

        let element = document.create_element(&self.tag).unwrap();
        let mut closures = HashMap::new();

        self.props.iter().for_each(|(name, value)| {
            element
                .set_attribute(name, value)
                .expect("Set element attribute in create element");
        });

        if self.events.0.len() > 0 {
            let unique_id = create_unique_identifier();

            element
                .set_attribute("data-vdom-id".into(), &unique_id.to_string())
                .expect("Could not set attribute on element");

            closures.insert(unique_id, vec![]);

            self.events.0.iter().for_each(|(onevent, callback)| {
                // onclick -> click
                let event = &onevent[2..];

                let current_elem: &EventTarget = element.dyn_ref().unwrap();

                current_elem
                    .add_event_listener_with_callback(
                        event,
                        callback.as_ref().as_ref().unchecked_ref(),
                    )
                    .unwrap();

                closures
                    .get_mut(&unique_id)
                    .unwrap()
                    .push(Rc::clone(callback));
            });
        }

        let mut previous_node_was_text = false;

        self.children.as_ref().unwrap().iter().for_each(|child| {
            match child {
                VirtualNode::Text(text_node) => {
                    let current_node = element.as_ref() as &web_sys::Node;

                    // We ensure that the text siblings are patched by preventing the browser from merging
                    // neighboring text nodes. Originally inspired by some of React's work from 2016.
                    //  -> https://reactjs.org/blog/2016/04/07/react-v15.html#major-changes
                    //  -> https://github.com/facebook/react/pull/5753
                    //
                    // `ptns` = Percy text node separator
                    if previous_node_was_text {
                        let separator = document.create_comment("ptns");
                        current_node
                            .append_child(separator.as_ref() as &web_sys::Node)
                            .unwrap();
                    }

                    current_node
                        .append_child(&text_node.create_text_node())
                        .unwrap();

                    previous_node_was_text = true;
                },
                VirtualNode::Element(element_node) => {
                    previous_node_was_text = false;

                    let child = element_node.create_element_node();
                    let child_elem = child.element;

                    closures.extend(child.closures);

                    element.append_child(&child_elem).unwrap();
                },
            }
        });

        CreatedElement { element, closures }
    }

}

impl VirtualNodeText {
    /// Return a `Text` element from a `VirtualNode`, typically right before adding it
    /// into the DOM.
    pub fn create_text_node(&self) -> Text {
        let document = web_sys::window().unwrap().document().unwrap();
        document.create_text_node(&self.text)
    }
}

/// A `web_sys::Element` along with all of the closures that were created for that element's
/// events and all of it's child element's events.
pub struct CreatedElement {
    /// An Element that was created from a VirtualNode
    pub element: Element,
    /// A map of an element's unique identifier along with all of the Closures for that element.
    ///
    /// The DomUpdater uses this to look up elements and see if they're still in the page. If not
    /// the reference that we maintain to their closure will be dropped, thus freeing the Closure's
    /// memory.
    pub closures: HashMap<u32, Vec<DynClosure>>,
}

impl Deref for CreatedElement {
    type Target = Element;
    fn deref(&self) -> &Self::Target {
        &self.element
    }
}

/// A `web_sys::Node` along with all of the closures that were created for that
/// node's events and all of it's child node's events.
pub struct CreatedNode {
    /// A Node` that was created from a `VirtualNode`
    pub node: Node,
    /// A map of a node's unique identifier along with all of the Closures for that node.
    ///
    /// The DomUpdater uses this to look up nodes and see if they're still in the page. If not
    /// the reference that we maintain to their closure will be dropped, thus freeing the Closure's
    /// memory.
    pub closures: HashMap<u32, Vec<DynClosure>>,
}

impl CreatedNode {
    pub fn without_closures<N: Into<Node>>(node: N) -> Self {
        CreatedNode {
            node: node.into(),
            closures: HashMap::with_capacity(0),
        }
    }
}

impl Deref for CreatedNode {
    type Target = Node;
    fn deref(&self) -> &Self::Target {
        &self.node
    }
}

impl Into<CreatedNode> for CreatedElement {
    fn into(self: Self) -> CreatedNode {
        CreatedNode {
            node: self.element.into(),
            closures: self.closures,
        }
    }
}

fn create_unique_identifier() -> u32 {
    let mut elem_unique_id = ELEM_UNIQUE_ID.lock().unwrap();

    *elem_unique_id += 1;

    *elem_unique_id
}

impl From<&str> for VirtualNode {
    fn from(text: &str) -> Self {
        VirtualNode::Text(text.into())
    }
}

impl From<String> for VirtualNode {
    fn from(text: String) -> Self {
        VirtualNode::Text(text.into())
    }
}

impl<'a> From<&'a String> for VirtualNode {
    fn from(text: &'a String) -> Self {
        VirtualNode::Text((&**text).into())
    }
}

impl From<&str> for VirtualNodeText {
    fn from(text: &str) -> Self {
        VirtualNodeText { text: text.to_string() }
    }
}

impl From<String> for VirtualNodeText {
    fn from(text: String) -> Self {
        VirtualNodeText { text }
    }
}

impl IntoIterator for VirtualNode {
    type Item = VirtualNode;
    // TODO: Is this possible with an array [VirtualNode] instead of a vec?
    type IntoIter = ::std::vec::IntoIter<VirtualNode>;

    fn into_iter(self) -> Self::IntoIter {
        vec![self].into_iter()
    }
}

impl fmt::Debug for VirtualNodeElement {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Element(<{}>, props: {:?}, children: {:?})",
            self.tag, self.props, self.children,
        )
    }
}

impl fmt::Debug for VirtualNodeText {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Text({})", self.text)
    }
}

impl fmt::Display for VirtualNodeElement {
    // Turn a VirtualNodeElement and all of it's children (recursively) into an HTML string
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "<{}", self.tag).unwrap();

        for (prop, value) in self.props.iter() {
            write!(f, r#" {}="{}""#, prop, value)?;
        }

        write!(f, ">")?;

        for child in self.children.as_ref().unwrap().iter() {
            write!(f, "{}", child.to_string())?;
        }

        if !self.is_self_closing() {
            write!(f, "</{}>", self.tag)?;
        }

        Ok(())
    }
}

// Turn a VirtualNodeText into an HTML string
impl fmt::Display for VirtualNodeText {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.text)
    }
}

// Turn a VirtualNode into an HTML string (delegate impl to variants)
impl fmt::Display for VirtualNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            VirtualNode::Element(element) => write!(f, "{}", element),
            VirtualNode::Text(text) => write!(f, "{}", text),
        }
    }
}

/// Box<dyn AsRef<JsValue>>> is our js_sys::Closure. Stored this way to allow us to store
/// any Closure regardless of the arguments.
pub type DynClosure = Rc<dyn AsRef<JsValue>>;

/// We need a custom implementation of fmt::Debug since JsValue doesn't
/// implement debug.
pub struct Events(pub HashMap<String, DynClosure>);

impl PartialEq for Events {
    // TODO: What should happen here..? And why?
    fn eq(&self, _rhs: &Self) -> bool {
        true
    }
}

impl fmt::Debug for Events {
    // Print out all of the event names for this VirtualNode
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let events: String = self.0.keys().map(|key| " ".to_string() + key).collect();
        write!(f, "{}", events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_closing_tag_to_string() {
        let node = VirtualNode::new("br");

        // No </br> since self closing tag
        assert_eq!(&node.to_string(), "<br>");
    }

    // TODO: Use html_macro as dev dependency and uncomment
    //    #[test]
    //    fn to_string() {
    //        let node = html! {
    //        <div id="some-id", !onclick=|_ev| {},>
    //            <span>
    //                { "Hello world" }
    //            </span>
    //        </div>
    //        };
    //        let expected = r#"<div id="some-id"><span>Hello world</span></div>"#;
    //
    //        assert_eq!(node.to_string(), expected);
    //    }
}
