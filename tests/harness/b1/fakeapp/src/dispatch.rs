//! Method handling: (path, interface, member, body) to a reply, against a [`World`].
//!
//! Pure with respect to I/O so it can be unit-tested without a bus.

use std::collections::HashMap;

use gorca_testkit::b1::proto::P2pMode;
use gorca_testkit::b1::world::{Item, World, iface, role, state_words};
use zvariant::{OwnedObjectPath, OwnedValue, Value};

pub const CACHE_PATH: &str = "/org/a11y/atspi/cache";
const NULL_PATH: &str = "/org/a11y/atspi/null";

pub type Ref = (String, OwnedObjectPath);
/// `((so)(so)(so)iiassusau)`.
pub type ItemWire = (
    Ref,
    Ref,
    Ref,
    i32,
    i32,
    Vec<String>,
    String,
    u32,
    String,
    Vec<u32>,
);

#[derive(Debug)]
pub enum Reply {
    Unit,
    Str(String),
    U32(u32),
    I32(i32),
    Ref(Ref),
    Refs(Vec<Ref>),
    Strs(Vec<String>),
    U32s(Vec<u32>),
    Rect((i32, i32, i32, i32)),
    StrMap(HashMap<String, String>),
    Relations(Vec<(u32, Vec<Ref>)>),
    Items(Vec<ItemWire>),
    Variant(OwnedValue),
    Props(HashMap<String, OwnedValue>),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DbusErr {
    UnknownObject,
    UnknownMethod,
    UnknownProperty,
    InvalidArgs,
}

impl DbusErr {
    pub fn name(self) -> &'static str {
        match self {
            DbusErr::UnknownObject => "org.freedesktop.DBus.Error.UnknownObject",
            DbusErr::UnknownMethod => "org.freedesktop.DBus.Error.UnknownMethod",
            DbusErr::UnknownProperty => "org.freedesktop.DBus.Error.UnknownProperty",
            DbusErr::InvalidArgs => "org.freedesktop.DBus.Error.InvalidArgs",
        }
    }
}

/// What the dispatcher needs to know about the process.
#[derive(Clone, Debug)]
pub struct Ctx {
    /// Our unique bus name (used in every reference we hand out).
    pub unique: String,
    /// Parent of the application root, from `Socket.Embed`.
    pub desktop: Ref,
    /// Answer of `GetApplicationBusAddress` (empty for `none`).
    pub app_bus_address: String,
    pub p2p: P2pMode,
    pub toolkit: String,
    /// IC-16 / MA-3: hostile refs; the value is the foreign bus name to plant.
    pub malform: Option<String>,
}

pub fn opath(p: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(p).unwrap_or_else(|_| {
        OwnedObjectPath::try_from(NULL_PATH).unwrap_or_else(|_| {
            OwnedObjectPath::from(zvariant::ObjectPath::from_static_str_unchecked(NULL_PATH))
        })
    })
}

fn null_ref() -> Ref {
    (String::new(), opath(NULL_PATH))
}

fn mk_ref(ctx: &Ctx, path: &str) -> Ref {
    (ctx.unique.clone(), opath(path))
}

fn variant<'a, T: Into<Value<'a>>>(v: T) -> Result<OwnedValue, DbusErr> {
    v.into().try_to_owned().map_err(|_| DbusErr::InvalidArgs)
}

fn role_name(r: u32) -> &'static str {
    match r {
        role::APPLICATION => "application",
        role::FRAME => "frame",
        role::PANEL => "panel",
        role::LABEL => "label",
        role::PUSH_BUTTON => "push button",
        role::CHECK_BOX => "check box",
        role::LIST => "list",
        role::LIST_ITEM => "list item",
        role::SLIDER => "slider",
        role::ENTRY => "entry",
        role::TOGGLE_BUTTON => "toggle button",
        role::TERMINAL => "terminal",
        role::DOCUMENT_WEB => "document web",
        role::IMAGE => "image",
        role::COMBO_BOX => "combo box",
        _ => "unknown",
    }
}

/// Plant hostile refs in a dump (IC-16): foreign parents, self-parents and a parent cycle.
fn malform_items(items: &mut [ItemWire], foreign: &str) {
    let paths: Vec<OwnedObjectPath> = items.iter().map(|i| i.0.1.clone()).collect();
    for (n, it) in items.iter_mut().enumerate() {
        match n % 17 {
            7 => it.2 = (foreign.to_owned(), paths[n].clone()),
            11 => it.2 = (it.0.0.clone(), paths[n].clone()),
            13 | 14 => {
                let other = if n % 17 == 13 { n + 1 } else { n - 1 };
                if let Some(p) = paths.get(other) {
                    it.2 = (it.0.0.clone(), p.clone());
                }
            }
            _ => {}
        }
    }
}

/// Wire form of a cache item.
pub fn item_wire(ctx: &Ctx, root_path: &str, it: &Item) -> ItemWire {
    let parent = match &it.parent {
        Some(p) => mk_ref(ctx, p),
        None if it.path == root_path => ctx.desktop.clone(),
        None => null_ref(),
    };
    (
        mk_ref(ctx, &it.path),
        mk_ref(ctx, root_path),
        parent,
        it.index,
        it.child_count,
        it.ifaces.iter().map(|s| (*s).to_owned()).collect(),
        it.name.clone(),
        it.role,
        it.desc.clone(),
        state_words(it.states).to_vec(),
    )
}

fn props_of(
    ctx: &Ctx,
    w: &World,
    path: &str,
    interface: &str,
) -> Result<HashMap<String, OwnedValue>, DbusErr> {
    let v = w.view(path).ok_or(DbusErr::UnknownObject)?;
    let mut m = HashMap::new();
    let mut put = |k: &str, val: OwnedValue| {
        m.insert(k.to_owned(), val);
    };
    match interface {
        "org.a11y.atspi.Accessible" => {
            put("Name", variant(v.name.clone())?);
            put("Description", variant(v.desc.clone())?);
            put("Locale", variant("C")?);
            put("AccessibleId", variant("")?);
            put("HelpText", variant("")?);
            put(
                "ChildCount",
                variant(i32::try_from(v.child_count).unwrap_or(i32::MAX))?,
            );
            let parent = match &v.parent {
                Some(p) => mk_ref(ctx, p),
                None => ctx.desktop.clone(),
            };
            put("Parent", variant(Value::from(parent))?);
        }
        "org.a11y.atspi.Value" if v.ifaces & iface::VALUE != 0 => {
            put("MinimumValue", variant(0.0f64)?);
            put("MaximumValue", variant(100.0f64)?);
            put("MinimumIncrement", variant(1.0f64)?);
            put("CurrentValue", variant(v.value.unwrap_or(0.0))?);
        }
        "org.a11y.atspi.Text" if v.ifaces & iface::TEXT != 0 => {
            put(
                "CharacterCount",
                variant(i32::try_from(v.name.chars().count()).unwrap_or(0))?,
            );
            put("CaretOffset", variant(0i32)?);
        }
        "org.a11y.atspi.Action" if v.ifaces & iface::ACTION != 0 => {
            put("NActions", variant(1i32)?);
        }
        "org.a11y.atspi.Application" if path == w.node(0).map_or("", |n| n.path.as_str()) => {
            put("ToolkitName", variant(ctx.toolkit.clone())?);
            put("Version", variant("1.0")?);
            put("AtspiVersion", variant("2.1")?);
            put("Id", variant(1i32)?);
        }
        "org.a11y.atspi.Component" => {}
        _ => return Err(DbusErr::UnknownProperty),
    }
    Ok(m)
}

/// The generic call arguments the dispatcher needs; decoded by the caller from the body.
#[derive(Debug, Default)]
pub struct Args {
    pub s1: Option<String>,
    pub s2: Option<String>,
    pub i1: Option<i32>,
    pub i2: Option<i32>,
    pub u1: Option<u32>,
}

/// Handle one call. `root_path` is the application root.
#[allow(clippy::too_many_lines)]
pub fn handle(
    ctx: &Ctx,
    w: &World,
    path: &str,
    interface: &str,
    member: &str,
    args: &Args,
    max_items: usize,
) -> Result<Reply, DbusErr> {
    let root_path = w.node(0).map_or("", |n| n.path.as_str());
    if interface == "org.freedesktop.DBus.Peer" {
        return match member {
            "Ping" => Ok(Reply::Unit),
            "GetMachineId" => Ok(Reply::Str("b1fakeapp".to_owned())),
            _ => Err(DbusErr::UnknownMethod),
        };
    }
    if path == CACHE_PATH {
        return match (interface, member) {
            ("org.a11y.atspi.Cache", "GetItems") => {
                let mut items: Vec<ItemWire> = w
                    .dump(max_items)
                    .iter()
                    .map(|i| item_wire(ctx, root_path, i))
                    .collect();
                if let Some(foreign) = &ctx.malform {
                    malform_items(&mut items, foreign);
                }
                Ok(Reply::Items(items))
            }
            _ => Err(DbusErr::UnknownMethod),
        };
    }
    let Some(node) = w.by_path(path) else {
        return Err(DbusErr::UnknownObject);
    };
    let view = w.view(path).ok_or(DbusErr::UnknownObject)?;
    match (interface, member) {
        ("org.freedesktop.DBus.Properties", "Get") => {
            let iface_name = args.s1.as_deref().ok_or(DbusErr::InvalidArgs)?;
            let prop = args.s2.as_deref().ok_or(DbusErr::InvalidArgs)?;
            let mut m = props_of(ctx, w, path, iface_name)?;
            m.remove(prop)
                .map(Reply::Variant)
                .ok_or(DbusErr::UnknownProperty)
        }
        ("org.freedesktop.DBus.Properties", "GetAll") => {
            let iface_name = args.s1.as_deref().ok_or(DbusErr::InvalidArgs)?;
            props_of(ctx, w, path, iface_name).map(Reply::Props)
        }
        ("org.a11y.atspi.Accessible", "GetRole") => Ok(Reply::U32(view.role)),
        ("org.a11y.atspi.Accessible", "GetRoleName" | "GetLocalizedRoleName") => {
            Ok(Reply::Str(role_name(view.role).to_owned()))
        }
        ("org.a11y.atspi.Accessible", "GetState") => {
            Ok(Reply::U32s(state_words(view.states).to_vec()))
        }
        ("org.a11y.atspi.Accessible", "GetChildren") => {
            let mut kids: Vec<Ref> = w
                .child_paths(path)
                .unwrap_or_default()
                .iter()
                .map(|p| mk_ref(ctx, p))
                .collect();
            if let Some(foreign) = &ctx.malform {
                // A child that lives in another application, one that is the node itself, and
                // a self-parented duplicate of the first child.
                kids.push((foreign.clone(), opath(path)));
                kids.push(mk_ref(ctx, path));
            }
            Ok(Reply::Refs(kids))
        }
        ("org.a11y.atspi.Accessible", "GetChildAtIndex") => {
            let i = usize::try_from(args.i1.ok_or(DbusErr::InvalidArgs)?)
                .map_err(|_| DbusErr::InvalidArgs)?;
            w.child_paths(path)
                .and_then(|c| c.get(i).map(|p| Reply::Ref(mk_ref(ctx, p))))
                .ok_or(DbusErr::InvalidArgs)
        }
        ("org.a11y.atspi.Accessible", "GetIndexInParent") => {
            let idx = node
                .parent
                .and_then(|p| w.node(p))
                .and_then(|p| p.children.iter().position(|c| *c == node.id))
                .and_then(|i| i32::try_from(i).ok())
                .unwrap_or(-1);
            Ok(Reply::I32(idx))
        }
        ("org.a11y.atspi.Accessible", "GetAttributes") => Ok(Reply::StrMap(HashMap::new())),
        ("org.a11y.atspi.Accessible", "GetInterfaces") => Ok(Reply::Strs(
            iface::names(view.ifaces)
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
        )),
        ("org.a11y.atspi.Accessible", "GetApplication") => Ok(Reply::Ref(mk_ref(ctx, root_path))),
        ("org.a11y.atspi.Accessible", "GetRelationSet") => Ok(Reply::Relations(Vec::new())),
        ("org.a11y.atspi.Component", "GetExtents") => Ok(Reply::Rect((0, 0, 100, 20))),
        ("org.a11y.atspi.Text", "GetText") if view.ifaces & iface::TEXT != 0 => {
            let (a, b) = (args.i1.unwrap_or(0), args.i2.unwrap_or(-1));
            let chars: Vec<char> = view.name.chars().collect();
            let end = if b < 0 {
                chars.len()
            } else {
                usize::try_from(b).unwrap_or(0).min(chars.len())
            };
            let start = usize::try_from(a).unwrap_or(0).min(end);
            Ok(Reply::Str(chars[start..end].iter().collect()))
        }
        ("org.a11y.atspi.Application", "GetApplicationBusAddress") if path == root_path => {
            Ok(Reply::Str(ctx.app_bus_address.clone()))
        }
        ("org.a11y.atspi.Application", "GetLocale") if path == root_path => {
            Ok(Reply::Str("C".to_owned()))
        }
        _ => Err(DbusErr::UnknownMethod),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_testkit::b1::proto::{ModelSpec, Shape};

    fn setup() -> (Ctx, World) {
        let spec = ModelSpec {
            shape: Shape::Settings,
            nodes_idle: 300,
            sidebar_rows: 10,
            ..ModelSpec::default()
        };
        let ctx = Ctx {
            unique: ":1.9".into(),
            desktop: (":1.1".into(), opath("/org/a11y/atspi/accessible/root")),
            app_bus_address: String::new(),
            p2p: P2pMode::None,
            toolkit: "GTK".into(),
            malform: None,
        };
        (ctx, World::new(spec, "s"))
    }

    #[test]
    fn get_role_name_and_children() {
        let (ctx, w) = setup();
        let root = w.node(0).unwrap().path.clone();
        let a = Args::default();
        assert!(matches!(
            handle(
                &ctx,
                &w,
                &root,
                "org.a11y.atspi.Accessible",
                "GetRole",
                &a,
                10
            ),
            Ok(Reply::U32(75))
        ));
        let Ok(Reply::Refs(kids)) = handle(
            &ctx,
            &w,
            &root,
            "org.a11y.atspi.Accessible",
            "GetChildren",
            &a,
            10,
        ) else {
            panic!("children");
        };
        assert_eq!(kids.len(), 1);
    }

    #[test]
    fn unknown_path_is_unknown_object_and_unknown_member_is_unknown_method() {
        let (ctx, w) = setup();
        let a = Args::default();
        assert_eq!(
            handle(
                &ctx,
                &w,
                "/org/a11y/atspi/accessible/nope",
                "org.a11y.atspi.Accessible",
                "GetRole",
                &a,
                1
            )
            .unwrap_err(),
            DbusErr::UnknownObject
        );
        let root = w.node(0).unwrap().path.clone();
        assert_eq!(
            handle(
                &ctx,
                &w,
                &root,
                "org.a11y.atspi.Accessible",
                "Frobnicate",
                &a,
                1
            )
            .unwrap_err(),
            DbusErr::UnknownMethod
        );
    }

    #[test]
    fn properties_get_and_getall() {
        let (ctx, w) = setup();
        let root = w.node(0).unwrap().path.clone();
        let a = Args {
            s1: Some("org.a11y.atspi.Accessible".into()),
            s2: Some("Name".into()),
            ..Args::default()
        };
        assert!(matches!(
            handle(
                &ctx,
                &w,
                &root,
                "org.freedesktop.DBus.Properties",
                "Get",
                &a,
                1
            ),
            Ok(Reply::Variant(_))
        ));
        let Ok(Reply::Props(m)) = handle(
            &ctx,
            &w,
            &root,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            &a,
            1,
        ) else {
            panic!("getall");
        };
        assert!(m.contains_key("ChildCount") && m.contains_key("Parent"));
    }

    #[test]
    fn get_items_respects_the_cap_and_the_cache_path() {
        let (ctx, w) = setup();
        let a = Args::default();
        let Ok(Reply::Items(v)) = handle(
            &ctx,
            &w,
            CACHE_PATH,
            "org.a11y.atspi.Cache",
            "GetItems",
            &a,
            50,
        ) else {
            panic!("items");
        };
        assert_eq!(v.len(), 50);
        assert_eq!(v[0].2.0, ":1.1", "the root's parent is the desktop");
    }
}
