import 'package:flutter/material.dart';
import 'package:get/get.dart';
import 'package:vento_annotation/annotations/required.dart';
import 'package:vento_annotation/index.dart';
import 'package:ventodemo/controllers/home_controller.dart';

import 'controllers/profile_controller.dart';
import 'repository/home_repository.dart';
import 'repository/profile_repository.dart';

part 'home_page.g.dart';

@HasPage(path: '/home', dependencies: [HomeController, ProfileController])
class HomePage extends StatelessWidget {
  @Required()
  final String? title;
  @Required(isRequired: false)
  final int? userId;

  const HomePage({super.key, this.title, this.userId});

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Home Page'),
      ),
      body: Center(
        child: Column(
          children: [
            Text('Title: $title'),
            Text('Title: $userId'),
          ],
        ),
      ),
    );
  }
}
