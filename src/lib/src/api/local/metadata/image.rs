//! Helper functions to get metadata from the images.
//!

use crate::error::OxenError;
use crate::model::entry::metadata_entry::{ImgColorSpace, MetaDataImage};

use image::GenericImageView;
use std::path::Path;

/// Detects the image metadata for the given file.
pub fn get_metadata(path: impl AsRef<Path>) -> Result<MetaDataImage, OxenError> {
    match image::open(path) {
        Ok(img) => {
            let (width, height) = img.dimensions();
            let color_space = image_to_colorspace(&img.color());
            Ok(MetaDataImage {
                width: width as usize,
                height: height as usize,
                color_space,
            })
        }
        Err(e) => {
            log::error!("Could not get image metadata {:?}", e);
            Err(OxenError::basic_str("Could not get image metadata"))
        }
    }
}

fn image_to_colorspace(colorspace: &image::ColorType) -> ImgColorSpace {
    match colorspace {
        image::ColorType::L8 => ImgColorSpace::Grayscale,
        image::ColorType::La8 => ImgColorSpace::GrayscaleAlpha,
        image::ColorType::Rgb8 => ImgColorSpace::RGB,
        image::ColorType::Rgba8 => ImgColorSpace::RGBA,
        image::ColorType::L16 => ImgColorSpace::Grayscale16,
        image::ColorType::La16 => ImgColorSpace::GrayscaleAlpha16,
        image::ColorType::Rgb16 => ImgColorSpace::Rgb16,
        image::ColorType::Rgba16 => ImgColorSpace::Rgba16,
        image::ColorType::Rgb32F => ImgColorSpace::Rgb32F,
        image::ColorType::Rgba32F => ImgColorSpace::Rgba32F,
        _ => ImgColorSpace::Unknown,
    }
}

#[cfg(test)]
mod tests {

    use crate::api;
    use crate::model::entry::entry_data_type::EntryDataType;
    use crate::model::entry::metadata_entry::ImgColorSpace;
    use crate::test;

    #[test]
    fn test_get_metadata_img_rgb() {
        let file = test::test_img_file_with_name("cat_1.jpg");

        let data = api::local::metadata::compute_metadata(file).unwrap();

        assert_eq!(data.data_type, EntryDataType::Image);
        assert_eq!(data.mime_type, "image/jpeg");

        let metadata = data.meta.image.unwrap();
        assert_eq!(metadata.width, 499);
        assert_eq!(metadata.height, 375);
        assert_eq!(metadata.color_space, ImgColorSpace::RGB);
    }

    #[test]
    fn test_get_metadata_img_rgba() {
        let file = test::test_img_file_with_name("cat_rgba.png");
        let data = api::local::metadata::compute_metadata(file).unwrap();

        assert_eq!(data.data_type, EntryDataType::Image);
        assert_eq!(data.mime_type, "image/png");

        let metadata = data.meta.image.unwrap();

        assert_eq!(metadata.width, 499);
        assert_eq!(metadata.height, 375);
        assert_eq!(metadata.color_space, ImgColorSpace::RGBA);
    }

    #[test]
    fn test_get_metadata_img_png_no_ext() {
        let file = test::test_img_file_with_name("cat_no_ext");
        let data = api::local::metadata::compute_metadata(file).unwrap();

        assert_eq!(data.data_type, EntryDataType::Image);
        assert_eq!(data.mime_type, "image/png");
        let metadata = data.meta.image.unwrap();

        assert_eq!(metadata.width, 499);
        assert_eq!(metadata.height, 375);
        assert_eq!(metadata.color_space, ImgColorSpace::RGBA);
    }

    #[test]
    fn test_get_metadata_img_grayscale() {
        let file = test::test_img_file_with_name("cat_grayscale.jpg");

        let data = api::local::metadata::compute_metadata(file).unwrap();

        assert_eq!(data.data_type, EntryDataType::Image);
        assert_eq!(data.mime_type, "image/jpeg");

        let metadata = data.meta.image.unwrap();

        assert_eq!(metadata.width, 499);
        assert_eq!(metadata.height, 375);
        assert_eq!(metadata.color_space, ImgColorSpace::Grayscale);
    }

    #[test]
    fn test_get_metadata_img_mnist() {
        let file = test::test_img_file_with_name("mnist_7.png");
        let data = api::local::metadata::compute_metadata(file).unwrap();

        assert_eq!(data.data_type, EntryDataType::Image);
        assert_eq!(data.mime_type, "image/png");

        let metadata = data.meta.image.unwrap();
        assert_eq!(metadata.width, 28);
        assert_eq!(metadata.height, 28);
        assert_eq!(metadata.color_space, ImgColorSpace::Grayscale);
    }
}
