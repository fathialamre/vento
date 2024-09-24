// GENERATED CODE - DO NOT MODIFY BY HAND

part of 'home_page.dart';

// **************************************************************************
// VentoGenerator
// **************************************************************************

class HomePageRoute {
  static GetPage getPage() {
    return GetPage(name: '/home', page: () => HomePage());
  }

  static void open({
    required String title,
    int? userId,
  }) {
    Get.to(HomePage());
  }
}

class HomePageBindings extends Bindings {
  @override
  void dependencies() {
    Get.lazyPut(() => HomeController(
        Get.find<HomeRepository>(), Get.find<ProfileRepository>()));
    Get.lazyPut<ProfileController>(() => ProfileController());
  }
}
