import 'package:get/get.dart';
import 'package:vento_annotation/annotations/has_repository.dart';
import 'package:ventodemo/repository/home_repository.dart';
import 'package:ventodemo/repository/profile_repository.dart';

@HasRepository(repositories: [HomeRepository, ProfileRepository])
class HomeController extends GetxController {
  final HomeRepository homeRepository;
  final ProfileRepository profileRepository;

  HomeController(this.homeRepository, this.profileRepository);

  getUsers() {
    print('Get users');
  }
}
