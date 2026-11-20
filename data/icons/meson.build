application_id = 'io.github.staffan.Euphonica'
legacy_application_id = 'io.github.htkhiem.Euphonica'

scalable_dir = 'hicolor' / 'scalable' / 'apps'
personal_scalable = configure_file(
  input: scalable_dir / ('@0@.svg').format(legacy_application_id),
  output: ('@0@.svg').format(application_id),
  copy: true,
)
install_data(
  personal_scalable,
  scalable_dir / ('@0@.svg').format(legacy_application_id),
  install_dir: get_option('datadir') / 'icons' / scalable_dir,
)

symbolic_dir = 'hicolor' / 'symbolic' / 'apps'
personal_symbolic = configure_file(
  input: symbolic_dir / ('@0@-symbolic.svg').format(legacy_application_id),
  output: ('@0@-symbolic.svg').format(application_id),
  copy: true,
)
install_data(
  personal_symbolic,
  symbolic_dir / ('@0@-symbolic.svg').format(legacy_application_id),
  install_dir: get_option('datadir') / 'icons' / symbolic_dir,
)
