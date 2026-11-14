import 'dart:async';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:material_ui/material_ui.dart';

import 'alerts.dart';
import 'cameras.dart';
import 'incidents.dart';
import 'onboarding.dart';
import 'profiles.dart';
import 'settings.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const StillWatchApp());
}

class StillWatchApp extends StatefulWidget {
  const StillWatchApp({super.key});

  @override
  State<StillWatchApp> createState() => _StillWatchAppState();
}

class _StillWatchAppState extends State<StillWatchApp>
    with WidgetsBindingObserver {
  static const _storage = FlutterSecureStorage();
  final _console = ConsoleState(const ProfileStore(_storage));
  late final _alerts = LocalAlerts(_console, _storage);

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    unawaited(
      _console.load().then((_) => mounted ? _alerts.initialize() : null),
    );
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      _alerts.refreshPermissionsOnResume();
    }
    unawaited(
      _console.setForeground(
        state == AppLifecycleState.resumed ||
            state == AppLifecycleState.inactive,
      ),
    );
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _alerts.dispose();
    _console.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => MaterialApp(
    debugShowCheckedModeBanner: false,
    title: 'StillWatch',
    theme: ThemeData(
      useMaterial3: true,
      brightness: Brightness.dark,
      scaffoldBackgroundColor: const Color(0xff101719),
      colorScheme: ColorScheme.fromSeed(
        seedColor: const Color(0xffa8c1a3),
        brightness: Brightness.dark,
        surface: const Color(0xff131d20),
        tertiary: const Color(0xffedc17a),
        error: const Color(0xffff9f8e),
      ),
      appBarTheme: const AppBarTheme(backgroundColor: Color(0xff101719)),
      inputDecorationTheme: const InputDecorationTheme(
        border: OutlineInputBorder(),
      ),
      dividerTheme: const DividerThemeData(color: Color(0xff2d3b3d)),
    ),
    home: _Console(_console, _alerts),
  );
}

class _Console extends StatefulWidget {
  const _Console(this.state, this.alerts);
  final ConsoleState state;
  final LocalAlerts alerts;

  @override
  State<_Console> createState() => _ConsoleState();
}

class _ConsoleState extends State<_Console> {
  int _page = 0;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.state,
    builder: (context, _) {
      final console = widget.state;
      final profile = console.selected;
      if (profile == null) return _FirstRun(console);
      final wide = MediaQuery.sizeOf(context).width >= 760;
      const labels = ['Cameras', 'Incidents', 'Settings'];
      final online = console.link == LinkState.connected;
      final view = switch (_page) {
        1 => IncidentHistory(key: ValueKey(profile.id), console: console),
        2 => CoreSettings(
          key: ValueKey(profile.id),
          console: console,
          alerts: widget.alerts,
        ),
        _ => CameraGrid(key: ValueKey(profile.id), console: console),
      };
      return Scaffold(
        appBar: AppBar(
          titleSpacing: 16,
          title: const Text('StillWatch'),
          actions: [
            if (wide) _CoreBadge(console.link),
            const SizedBox(width: 8),
            PopupMenuButton<CoreProfile>(
              tooltip: 'Switch Core',
              onSelected: (profile) {
                unawaited(console.select(profile));
                setState(() => _page = 0);
              },
              itemBuilder: (_) => [
                for (final candidate in console.profiles)
                  CheckedPopupMenuItem(
                    value: candidate,
                    checked: candidate == profile,
                    child: Text(candidate.name),
                  ),
              ],
              child: Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: 10,
                  vertical: 10,
                ),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const Icon(Icons.dns_outlined, size: 18),
                    const SizedBox(width: 7),
                    ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: 112),
                      child: Text(
                        profile.name,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                    const Icon(Icons.expand_more, size: 18),
                  ],
                ),
              ),
            ),
            IconButton(
              onPressed: () => addCore(context, console),
              tooltip: 'Add a Core',
              icon: const Icon(Icons.add_link),
            ),
            const SizedBox(width: 6),
          ],
        ),
        bottomNavigationBar: wide
            ? null
            : NavigationBar(
                selectedIndex: _page,
                onDestinationSelected: (page) => setState(() => _page = page),
                destinations: const [
                  NavigationDestination(
                    icon: Icon(Icons.grid_view_outlined),
                    label: 'Cameras',
                  ),
                  NavigationDestination(
                    icon: Icon(Icons.notifications_none),
                    label: 'Incidents',
                  ),
                  NavigationDestination(
                    icon: Icon(Icons.tune),
                    label: 'Settings',
                  ),
                ],
              ),
        body: Row(
          children: [
            if (wide) ...[
              NavigationRail(
                backgroundColor: const Color(0xff131d20),
                labelType: NavigationRailLabelType.all,
                selectedIndex: _page,
                onDestinationSelected: (page) => setState(() => _page = page),
                destinations: const [
                  NavigationRailDestination(
                    icon: Icon(Icons.grid_view_outlined),
                    label: Text('Cameras'),
                  ),
                  NavigationRailDestination(
                    icon: Icon(Icons.notifications_none),
                    label: Text('Incidents'),
                  ),
                  NavigationRailDestination(
                    icon: Icon(Icons.tune),
                    label: Text('Settings'),
                  ),
                ],
              ),
              const VerticalDivider(width: 1),
            ],
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Padding(
                    padding: const EdgeInsets.fromLTRB(20, 12, 20, 2),
                    child: Wrap(
                      alignment: WrapAlignment.spaceBetween,
                      crossAxisAlignment: WrapCrossAlignment.center,
                      spacing: 12,
                      runSpacing: 8,
                      children: [
                        Text(
                          labels[_page],
                          style: Theme.of(context).textTheme.headlineMedium,
                        ),
                        if (_page == 0 && online && profile.role == 'owner')
                          OutlinedButton.icon(
                            onPressed: () => editCamera(context, console),
                            icon: const Icon(Icons.add),
                            label: const Text('Camera'),
                          ),
                        if (!wide) _CoreBadge(console.link),
                      ],
                    ),
                  ),
                  if (console.error != null)
                    Padding(
                      padding: const EdgeInsets.fromLTRB(20, 8, 20, 0),
                      child: Row(
                        children: [
                          Expanded(
                            child: Text(
                              console.error!,
                              style: TextStyle(
                                color: Theme.of(context).colorScheme.tertiary,
                              ),
                            ),
                          ),
                          if (console.link == LinkState.offline)
                            TextButton(
                              onPressed: console.reconnect,
                              child: const Text('Retry'),
                            ),
                        ],
                      ),
                    ),
                  if (console.link == LinkState.connecting)
                    const Padding(
                      padding: EdgeInsets.fromLTRB(20, 10, 20, 0),
                      child: LinearProgressIndicator(),
                    ),
                  Expanded(child: view),
                ],
              ),
            ),
          ],
        ),
      );
    },
  );
}

class _CoreBadge extends StatelessWidget {
  const _CoreBadge(this.state);
  final LinkState state;
  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final online = state == LinkState.connected;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Icon(
          Icons.circle,
          size: 8,
          color: online ? colors.primary : colors.tertiary,
        ),
        const SizedBox(width: 6),
        Text(switch (state) {
          LinkState.connected => 'Core connected',
          LinkState.connecting => 'Connecting',
          LinkState.offline => 'Core offline',
          LinkState.untrusted => 'Unverified Core',
          LinkState.revoked => 'Pairing required',
        }, style: Theme.of(context).textTheme.labelMedium),
      ],
    );
  }
}

class _FirstRun extends StatelessWidget {
  const _FirstRun(this.console);
  final ConsoleState console;
  @override
  Widget build(BuildContext context) => Scaffold(
    body: Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(32),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Icon(
                Icons.visibility_outlined,
                size: 48,
                color: Theme.of(context).colorScheme.primary,
              ),
              const SizedBox(height: 20),
              Text(
                'StillWatch',
                style: Theme.of(context).textTheme.displaySmall,
              ),
              const SizedBox(height: 8),
              const Text(
                'Connect to a Core to view its cameras and incidents.',
              ),
              if (console.error != null) ...[
                const SizedBox(height: 16),
                Text(console.error!),
              ],
              const SizedBox(height: 22),
              if (console.loading)
                const CircularProgressIndicator()
              else if (console.error != null)
                OutlinedButton(
                  onPressed: console.load,
                  child: const Text('Retry secure storage'),
                )
              else
                FilledButton.icon(
                  onPressed: () => addCore(context, console),
                  icon: const Icon(Icons.add_link),
                  label: const Text('Add a Core'),
                ),
            ],
          ),
        ),
      ),
    ),
  );
}
