import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/desktop/pages/desktop_home_page.dart';
import 'package:flutter_hbb/desktop/pages/desktop_setting_page.dart';
import 'package:flutter_hbb/desktop/pages/onyx/onyx_page.dart';
import 'package:flutter_hbb/desktop/widgets/tabbar_widget.dart';
import 'package:flutter_hbb/models/platform_model.dart';
import 'package:flutter_hbb/models/state_model.dart';
import 'package:get/get.dart';
import 'package:window_manager/window_manager.dart';
// import 'package:flutter/services.dart';

import '../../common/shared_state.dart';

class DesktopTabPage extends StatefulWidget {
  const DesktopTabPage({Key? key}) : super(key: key);

  @override
  State<DesktopTabPage> createState() => _DesktopTabPageState();

  static void onAddSetting(
      {SettingsTabKey initialPage = SettingsTabKey.general}) {
    try {
      DesktopTabController tabController = Get.find<DesktopTabController>();
      tabController.add(TabInfo(
          key: kTabLabelSettingPage,
          label: kTabLabelSettingPage,
          selectedIcon: Icons.build_sharp,
          unselectedIcon: Icons.build_outlined,
          page: DesktopSettingPage(
            key: const ValueKey(kTabLabelSettingPage),
            initialTabkey: initialPage,
          )));
    } catch (e) {
      debugPrintStack(label: '$e');
    }
  }

  /// ONYX 控制台：设备列表 / 处置 / 静默远控。
  /// `add()` 自带同 key 去重，重复点只会切过去，不会开第二个。
  static void onAddOnyx() {
    try {
      DesktopTabController tabController = Get.find<DesktopTabController>();
      tabController.add(TabInfo(
          key: kTabLabelOnyxPage,
          label: kTabLabelOnyxPage,
          selectedIcon: Icons.shield_sharp,
          unselectedIcon: Icons.shield_outlined,
          page: const OnyxPage(key: ValueKey(kTabLabelOnyxPage))));
    } catch (e) {
      debugPrintStack(label: '$e');
    }
  }
}

class _DesktopTabPageState extends State<DesktopTabPage> {
  final tabController = DesktopTabController(tabType: DesktopTabType.main);

  _DesktopTabPageState() {
    RemoteCountState.init();
    Get.put<DesktopTabController>(tabController);
    tabController.add(TabInfo(
        key: kTabLabelHomePage,
        label: kTabLabelHomePage,
        selectedIcon: Icons.home_sharp,
        unselectedIcon: Icons.home_outlined,
        closable: false,
        page: DesktopHomePage(
          key: const ValueKey(kTabLabelHomePage),
        )));
    if (bind.isIncomingOnly()) {
      tabController.onSelected = (key) {
        if (key == kTabLabelHomePage) {
          windowManager.setSize(getIncomingOnlyHomeSize());
          setResizable(false);
        } else {
          windowManager.setSize(getIncomingOnlySettingsSize());
          setResizable(true);
        }
      };
    }
    // ONYX 控制台作为常驻标签页：操作员一眼可见，不必去找角落里的图标。
    // add() 内部会 jumpTo 选中新标签，所以加完再切回主页，避免每次启动都落在 ONYX。
    tabController.add(TabInfo(
        key: kTabLabelOnyxPage,
        label: kTabLabelOnyxPage,
        selectedIcon: Icons.shield_sharp,
        unselectedIcon: Icons.shield_outlined,
        closable: false,
        page: const OnyxPage(key: ValueKey(kTabLabelOnyxPage))));
    try {
      tabController.jumpTo(0);
    } catch (e) {
      debugPrint('jumpTo home failed: $e');
    }
  }

  @override
  void initState() {
    super.initState();
    // HardwareKeyboard.instance.addHandler(_handleKeyEvent);
  }

  /*
  bool _handleKeyEvent(KeyEvent event) {
    if (!mouseIn && event is KeyDownEvent) {
      print('key down: ${event.logicalKey}');
      shouldBeBlocked(_block, canBeBlocked);
    }
    return false; // allow it to propagate
  }
  */

  @override
  void dispose() {
    // HardwareKeyboard.instance.removeHandler(_handleKeyEvent);
    Get.delete<DesktopTabController>();

    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final tabWidget = Container(
        child: Scaffold(
            backgroundColor: Theme.of(context).colorScheme.background,
            body: DesktopTab(
              controller: tabController,
              tail: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  // ONYX 控制台入口。放在设置左边，随时一键进设备列表。
                  ActionIcon(
                    message: 'ONYX',
                    // 用 Material 内置盾牌，不用 IconFont.deviceGroupOutline：
                    // 自定义字体在运行时渲染成豆腐块（2026-10-31 实测）。
                    icon: Icons.shield_outlined,
                    onTap: DesktopTabPage.onAddOnyx,
                    isClose: false,
                  ),
                  Offstage(
                    offstage: bind.isIncomingOnly() || bind.isDisableSettings(),
                    child: ActionIcon(
                      message: 'Settings',
                      icon: IconFont.menu,
                      onTap: DesktopTabPage.onAddSetting,
                      isClose: false,
                    ),
                  ),
                ],
              ),
            )));
    return isMacOS || kUseCompatibleUiMode
        ? tabWidget
        : Obx(
            () => DragToResizeArea(
              resizeEdgeSize: stateGlobal.resizeEdgeSize.value,
              enableResizeEdges: windowManagerEnableResizeEdges,
              child: tabWidget,
            ),
          );
  }
}
