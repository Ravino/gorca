// gorca-helper@gorca: thin loader. All logic lives in helper.js.
// If a file named `.dev` exists next to this file, helper.js is re-imported with a
// cache-busting query string on every enable(), so edits take effect without
// restarting the shell (GNOME Shell caches ES modules for its whole lifetime).
import Gio from 'gi://Gio';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

export default class GorcaHelperExtension extends Extension {
    enable() {
        this._enabled = true;
        this._load().catch(e => console.error(`gorca-helper: load failed: ${e}\n${e.stack}`));
    }

    async _load() {
        let suffix = '';
        const dev = this.dir.get_child('.dev');
        if (dev.query_exists(null)) {
            const info = this.dir.get_child('helper.js')
                .query_info('time::modified-usec,time::modified', 0, null);
            suffix = `?v=${info.get_attribute_uint64('time::modified')}.${info.get_attribute_uint32('time::modified-usec')}`;
        }
        const mod = await import(`./helper.js${suffix}`);
        if (!this._enabled)
            return;
        this._service = new mod.ShellHelperService(this.dir.get_path());
        this._service.start();
    }

    disable() {
        this._enabled = false;
        this._service?.stop();
        this._service = null;
    }
}
