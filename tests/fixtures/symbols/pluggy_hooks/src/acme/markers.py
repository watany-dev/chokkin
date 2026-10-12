import pluggy

hookimpl = pluggy.HookimplMarker("acme")
pm = pluggy.PluginManager("acme")
