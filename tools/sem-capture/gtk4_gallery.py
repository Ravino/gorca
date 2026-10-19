#!/usr/bin/python3
"""GTK4 widget gallery for the semantic fixture corpus (A2).

  gtk4_gallery.py --list          print the page names
  gtk4_gallery.py <page>          show one page until killed

GTK4 exports no per-widget id over AT-SPI, so every gallery widget carries its fixture id in its
accessible DESCRIPTION as "fx:<id>" or "fx:<id>|<real description>". capture.py does not interpret
this: the A2 consumer (expect.toml) keys GTK4 rows by that prefix. Widgets that test description
semantics use the "|" form. Use the system Python (PyGObject), not the project venv.
"""
import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gio, GLib, Gtk  # noqa: E402

PAGES = ["controls", "containers", "accprops"]
AP = Gtk.AccessibleProperty
AS = Gtk.AccessibleState
AR = Gtk.AccessibleRole
TRI = Gtk.AccessibleTristate


def tag(w, fid, desc=None):
    w.update_property([AP.DESCRIPTION], ["fx:%s%s" % (fid, "|" + desc if desc else "")])
    return w


def vbox(*children):
    b = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
    for c in children:
        b.append(c)
    return b


def page_controls():
    ls = Gtk.StringList.new(["Red", "Green", "Blue"])
    check = Gtk.CheckButton(label="Check unchecked")
    check_on = Gtk.CheckButton(label="Check checked")
    check_on.set_active(True)
    check_mixed = Gtk.CheckButton(label="Check inconsistent")
    check_mixed.set_inconsistent(True)
    r1 = Gtk.CheckButton(label="Radio A")
    r2 = Gtk.CheckButton(label="Radio B")
    r2.set_group(r1)
    r1.set_active(True)
    tog = Gtk.ToggleButton(label="Toggle off")
    tog_on = Gtk.ToggleButton(label="Toggle on")
    tog_on.set_active(True)
    sw = Gtk.Switch()
    sw.update_property([AP.LABEL], ["Switch off"])
    sw_on = Gtk.Switch()
    sw_on.set_active(True)
    sw_on.update_property([AP.LABEL], ["Switch on"])
    entry = Gtk.Entry(placeholder_text="Placeholder text")
    entry_label = Gtk.Entry()
    entry_label.update_property([AP.LABEL], ["Named entry"])
    entry_ro = Gtk.Entry(editable=False, text="read only")
    entry_ro.update_property([AP.LABEL], ["Readonly entry"])
    entry_disabled = Gtk.Entry(sensitive=False, text="off")
    pw = Gtk.PasswordEntry()
    pw.update_property([AP.LABEL], ["Password entry"])
    se = Gtk.SearchEntry()
    spin = Gtk.SpinButton.new_with_range(0, 10, 1)
    spin.set_value(3)
    scale = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 0, 100, 1)
    scale.set_value(40)
    scale.update_property([AP.LABEL], ["Volume scale"])
    scale_v = Gtk.Scale.new_with_range(Gtk.Orientation.VERTICAL, 0, 10, 1)
    scale_v.update_property([AP.LABEL], ["Vertical scale"])
    prog = Gtk.ProgressBar(fraction=0.25)
    prog.update_property([AP.LABEL], ["Progress"])
    prog_pulse = Gtk.ProgressBar()
    prog_pulse.pulse()
    lvl = Gtk.LevelBar.new_for_interval(0, 10)
    lvl.set_value(3)
    lvl.update_property([AP.LABEL], ["Level"])
    label = Gtk.Label(label="Plain label")
    label_sel = Gtk.Label(label="Selectable label", selectable=True)
    link = Gtk.LinkButton.new_with_label("https://example.org", "Link button")
    btn = Gtk.Button(label="Push button")
    btn_icon = Gtk.Button(icon_name="document-save-symbolic")
    btn_icon.update_property([AP.LABEL], ["Icon button"])
    btn_icon_noname = Gtk.Button(icon_name="edit-delete-symbolic")
    btn_off = Gtk.Button(label="Disabled button", sensitive=False)
    dd = Gtk.DropDown(model=ls)
    dd.update_property([AP.LABEL], ["Colour drop-down"])
    cal = Gtk.Calendar()
    tv = Gtk.TextView()
    tv.get_buffer().set_text("Multi-line text")
    tv.update_property([AP.LABEL], ["Text view"])
    pic = Gtk.Picture()
    pic.set_alternative_text("Picture alt")
    img = Gtk.Image.new_from_icon_name("dialog-information-symbolic")
    img.update_property([AP.LABEL], ["Info icon"])
    sep = Gtk.Separator()
    sepv = Gtk.Separator(orientation=Gtk.Orientation.VERTICAL)
    spinner = Gtk.Spinner(spinning=True)
    for fid, w in [("btn-push", btn), ("btn-icon", btn_icon), ("btn-icon-noname", btn_icon_noname),
                   ("btn-disabled", btn_off), ("btn-link", link),
                   ("tog-off", tog), ("tog-on", tog_on), ("cb-off", check), ("cb-on", check_on),
                   ("cb-mixed", check_mixed), ("radio-a", r1), ("radio-b", r2),
                   ("sw-off", sw), ("sw-on", sw_on), ("entry-ph", entry), ("entry-named", entry_label),
                   ("entry-ro", entry_ro), ("entry-disabled", entry_disabled), ("entry-pw", pw),
                   ("entry-search", se), ("spin", spin), ("scale-h", scale), ("scale-v", scale_v),
                   ("prog-frac", prog), ("prog-pulse", prog_pulse), ("level", lvl), ("label", label),
                   ("label-sel", label_sel), ("dropdown", dd), ("calendar", cal), ("textview", tv),
                   ("picture", pic), ("image", img), ("sep-h", sep), ("sep-v", sepv), ("spinner", spinner)]:
        tag(w, fid)
    return vbox(btn, btn_icon, btn_icon_noname, btn_off, link, tog, tog_on, check, check_on, check_mixed,
                r1, r2, sw, sw_on, entry, entry_label, entry_ro, entry_disabled, pw, se, spin, scale,
                scale_v, prog, prog_pulse, lvl, label, label_sel, dd, cal, tv, pic, img, sep, sepv, spinner)


def page_containers():
    nb = Gtk.Notebook()
    nb.append_page(Gtk.Label(label="Page one body"), Gtk.Label(label="Tab one"))
    nb.append_page(Gtk.Label(label="Page two body"), Gtk.Label(label="Tab two"))
    tag(nb, "notebook")
    ex = Gtk.Expander(label="Expander closed")
    ex.set_child(Gtk.Label(label="inside"))
    ex_open = Gtk.Expander(label="Expander open", expanded=True)
    ex_open.set_child(Gtk.Label(label="inside open"))
    tag(ex, "expander-closed")
    tag(ex_open, "expander-open")
    fr = Gtk.Frame(label="Frame label")
    fr.set_child(Gtk.Label(label="in frame"))
    tag(fr, "frame")
    pn = Gtk.Paned(orientation=Gtk.Orientation.HORIZONTAL)
    pn.set_start_child(Gtk.Label(label="start"))
    pn.set_end_child(Gtk.Label(label="end"))
    tag(pn, "paned")
    lb = Gtk.ListBox()
    for i in range(3):
        row = Gtk.ListBoxRow()
        row.set_child(Gtk.Label(label="Row %d" % (i + 1)))
        tag(row, "lb-row-%d" % (i + 1))
        lb.append(row)
    lb.select_row(lb.get_row_at_index(1))
    tag(lb, "listbox")
    store = Gtk.StringList.new(["Alpha", "Beta", "Gamma"])
    sel = Gtk.SingleSelection(model=store)
    fac = Gtk.SignalListItemFactory()
    fac.connect("setup", lambda f, li: li.set_child(Gtk.Label()))
    fac.connect("bind", lambda f, li: (li.get_child().set_label(li.get_item().get_string()),
                                       tag(li.get_child().get_parent() or li.get_child(), "lv-item-%d" % li.get_position())))
    lv = Gtk.ListView(model=sel, factory=fac)
    tag(lv, "listview")
    cv_model = Gtk.SingleSelection(model=Gtk.StringList.new(["r1", "r2"]))
    cv = Gtk.ColumnView(model=cv_model)
    cfac = Gtk.SignalListItemFactory()
    cfac.connect("setup", lambda f, li: li.set_child(Gtk.Label()))
    cfac.connect("bind", lambda f, li: li.get_child().set_label(li.get_item().get_string()))
    col = Gtk.ColumnViewColumn(title="Column A", factory=cfac)
    cv.append_column(col)
    tag(cv, "columnview")
    tb = Gtk.MenuButton(label="Menu button")
    menu = Gio.Menu()
    menu.append("First action", "app.first")
    menu.append("Second action", "app.second")
    tb.set_menu_model(menu)
    tag(tb, "menubutton")
    stack = Gtk.Stack()
    stack.add_titled(Gtk.Label(label="Stack page A"), "a", "Page A")
    stack.add_titled(Gtk.Label(label="Stack page B"), "b", "Page B")
    sw = Gtk.StackSwitcher(stack=stack)
    tag(stack, "stack")
    tag(sw, "stackswitcher")
    ib = Gtk.InfoBar(message_type=Gtk.MessageType.WARNING)
    ib.add_child(Gtk.Label(label="Info bar text"))
    tag(ib, "infobar")
    ab = Gtk.ActionBar()
    ab.pack_start(Gtk.Button(label="Action bar button"))
    tag(ab, "actionbar")
    sc = Gtk.ScrolledWindow()
    sc.set_child(Gtk.Label(label="scrolled"))
    sc.set_min_content_height(40)
    tag(sc, "scrolled")
    fb = Gtk.FlowBox()
    fb.append(Gtk.Label(label="flow 1"))
    fb.append(Gtk.Label(label="flow 2"))
    tag(fb, "flowbox")
    grid = Gtk.Grid()
    grid.attach(Gtk.Label(label="g00"), 0, 0, 1, 1)
    grid.attach(Gtk.Label(label="g10"), 1, 0, 1, 1)
    tag(grid, "grid")
    rv = Gtk.Revealer(reveal_child=True)
    rv.set_child(Gtk.Label(label="revealed"))
    tag(rv, "revealer")
    ovl = Gtk.Overlay()
    ovl.set_child(Gtk.Label(label="overlay base"))
    tag(ovl, "overlay")
    return vbox(nb, ex, ex_open, fr, pn, lb, lv, cv, tb, stack, sw, ib, ab, sc, fb, grid, rv, ovl)


def page_accprops():
    out = []

    def mk(fid, widget, props=(), states=()):
        for k, v in props:
            if isinstance(v, (Gtk.AccessibleAutocomplete, Gtk.AccessibleSort)):
                v = int(v)
            widget.update_property([k], [v])
        for k, v in states:
            if isinstance(v, Gtk.AccessibleInvalidState):
                v = int(v)
            widget.update_state([k], [v])
        tag(widget, fid)
        out.append(widget)
        return widget

    mk("ap-label", Gtk.Button(label="visible"), [(AP.LABEL, "Accessible label")])
    mk("ap-desc", Gtk.Button(label="has desc"))  # description set below with real text
    out[-1].update_property([AP.DESCRIPTION], ["fx:ap-desc|Real description text"])
    # update_relation() with a widget list is not usable from PyGObject (GList* boxed as pointer);
    # the mnemonic widget is the GTK4 route to a label-for / labelled-by relation.
    tgt = Gtk.Entry()
    mk("ap-labelledby-tgt", tgt)
    src = Gtk.Label(label="_Label source", use_underline=True, mnemonic_widget=tgt)
    mk("ap-labelledby-src", src)
    mk("ap-required", Gtk.Entry(), [(AP.LABEL, "Required entry"), (AP.REQUIRED, True)] if hasattr(AP, "REQUIRED") else [(AP.LABEL, "Required entry")])
    mk("ap-invalid", Gtk.Entry(), [(AP.LABEL, "Invalid entry")], [(AS.INVALID, Gtk.AccessibleInvalidState.TRUE)])
    mk("ap-invalid-grammar", Gtk.Entry(), [(AP.LABEL, "Invalid grammar")], [(AS.INVALID, Gtk.AccessibleInvalidState.GRAMMAR)])
    mk("ap-roledesc", Gtk.Button(label="rd"), [(AP.ROLE_DESCRIPTION, "super button")])
    mk("ap-keyshortcuts", Gtk.Button(label="ks"), [(AP.KEY_SHORTCUTS, "Control+K")])
    mk("ap-haspopup", Gtk.Button(label="popup"), [(AP.HAS_POPUP, True)])
    mk("ap-placeholder", Gtk.Entry(), [(AP.LABEL, "Has placeholder"), (AP.PLACEHOLDER, "ph text")])
    mk("ap-autocomplete", Gtk.Entry(), [(AP.LABEL, "Autocomplete"), (AP.AUTOCOMPLETE, Gtk.AccessibleAutocomplete.LIST)])
    mk("ap-level", Gtk.Label(label="Heading label"), [(AP.LEVEL, 2)])
    mk("ap-multiline", Gtk.TextView(), [(AP.LABEL, "ML"), (AP.MULTI_LINE, True)])
    mk("ap-orient-v", Gtk.Box(), [(AP.ORIENTATION, Gtk.Orientation.VERTICAL)])
    mk("ap-valuetext", Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 0, 6, 1), [(AP.LABEL, "Day"), (AP.VALUE_TEXT, "Tuesday")])
    mk("ap-busy", Gtk.Label(label="busy label"), [], [(AS.BUSY, True)])
    mk("ap-pressed-mixed", Gtk.ToggleButton(label="pressed mixed"), [], [(AS.PRESSED, TRI.MIXED)])
    mk("ap-checked-mixed", Gtk.CheckButton(label="checked mixed"), [], [(AS.CHECKED, TRI.MIXED)])
    mk("ap-expanded-true", Gtk.Button(label="expanded"), [], [(AS.EXPANDED, True)])
    mk("ap-selected-true", Gtk.Button(label="selected"), [], [(AS.SELECTED, True)])
    mk("ap-disabled", Gtk.Button(label="aria disabled"), [], [(AS.DISABLED, True)])
    mk("ap-hidden", Gtk.Button(label="hidden state"), [], [(AS.HIDDEN, True)])
    mk("ap-readonly", Gtk.Entry(), [(AP.LABEL, "RO"), (AP.READ_ONLY, True)] if hasattr(AP, "READ_ONLY") else [(AP.LABEL, "RO")])
    mk("ap-modal", Gtk.Box(), [(AP.MODAL, True)] if hasattr(AP, "MODAL") else [])
    mk("ap-sort", Gtk.Label(label="sorted col"), [(AP.SORT, Gtk.AccessibleSort.ASCENDING)] if hasattr(AP, "SORT") else [])
    # custom roles through accessible-role (construct-only)
    for fid, rname in [('role-toolbar', 'TOOLBAR'), ('role-tablist', 'TAB_LIST'), ('role-menubar', 'MENU_BAR'), ('role-group', 'GROUP'), ('role-article', 'ARTICLE'), ('role-banner', 'BANNER'), ('role-navigation', 'NAVIGATION'), ('role-main', 'MAIN'), ('role-search', 'SEARCH'), ('role-status', 'STATUS'), ('role-alert', 'ALERT'), ('role-log', 'LOG'), ('role-timer', 'TIMER'), ('role-marquee', 'MARQUEE'), ('role-region', 'GROUP'), ('role-dialog', 'DIALOG'), ('role-tooltip', 'TOOLTIP'), ('role-heading', 'HEADING'), ('role-paragraph', 'PARAGRAPH'), ('role-list', 'LIST'), ('role-feed', 'FEED'), ('role-figure', 'FIGURE'), ('role-document', 'DOCUMENT'), ('role-application', 'APPLICATION'), ('role-generic', 'GENERIC'), ('role-presentation', 'PRESENTATION'), ('role-img', 'IMG'), ('role-meter', 'METER'), ('role-term', 'TERM'), ('role-definition', 'DEFINITION'), ('role-code', 'CODE'), ('role-blockquote', 'BLOCK_QUOTE'), ('role-caption', 'CAPTION'), ('role-comment', 'COMMENT'), ('role-math', 'MATH'), ('role-insertion', 'INSERTION'), ('role-deletion', 'DELETION'), ('role-mark', 'MARK'), ('role-time', 'TIME'), ('role-emphasis', 'EMPHASIS'), ('role-strong', 'STRONG'), ('role-subscript', 'SUBSCRIPT'), ('role-superscript', 'SUPERSCRIPT'), ('role-note', 'NOTE'), ('role-rowgroup', 'ROW_GROUP'), ('role-scrollbar', 'SCROLLBAR'), ('role-tree', 'TREE'), ('role-treegrid', 'TREE_GRID'), ('role-grid', 'GRID'), ('role-table', 'TABLE'), ('role-menu', 'MENU')]:
        role = getattr(AR, rname, None)
        if role is None:
            print("skip", fid, file=sys.stderr)
            continue
        try:
            w = Gtk.Box(accessible_role=role)
            w.append(Gtk.Label(label="text in " + fid))
            w.update_property([AP.LABEL], ["Label " + fid])
            tag(w, fid)
            out.append(w)
        except Exception as e:  # role missing in this GTK
            print("skip", fid, e, file=sys.stderr)
    return vbox(*out)


BUILD = {"controls": page_controls, "containers": page_containers, "accprops": page_accprops}


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "--list":
        print("\n".join(PAGES))
        return
    page = sys.argv[1]
    GLib.set_prgname("sem-gallery")
    app = Gtk.Application(application_id="org.gorca.SemGallery")

    def activate(a):
        win = Gtk.ApplicationWindow(application=a, title="SEM gtk4 " + page, default_width=900, default_height=700)
        win.set_child(BUILD[page]())
        win.present()

    app.connect("activate", activate)
    app.run([])


main()
