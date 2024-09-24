import 'package:flutter/material.dart';
import 'package:ventodemo/home_page.dart';

class MainLayoutPage extends StatelessWidget {
  const MainLayoutPage({super.key});

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text('MainLayoutPage'),
      ),
      body: Center(
        child: Column(
          children: [
            ElevatedButton(
                onPressed: () {
                  HomePageRoute.open(title: 'Home Page HERE', userId: 1 );
                },
                child: Text("HOME"))
          ],
        ),
      ),
    );
  }
}
