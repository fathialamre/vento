import 'package:flutter/material.dart';
import 'package:get/get.dart';
import 'package:ventodemo/home_page.dart';
import 'package:ventodemo/pages/main_layout.dart';

void main() {
  runApp(const MyApp());
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  // This widget is the root of your application.
  @override
  Widget build(BuildContext context) {
    return GetMaterialApp(
      title: 'Flutter Demo',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.deepPurple),
        useMaterial3: true,
      ),
      initialRoute: '/main',
      getPages: [
        GetPage(name: '/main', page: () => const MainLayoutPage()),
        HomePageRoute.getPage(),
      ],
    );
  }
}

